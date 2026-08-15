//! Driving a body's engine-authored motion, and a window for steering it.
//!
//! Every number here that decides how a body *moves* comes from
//! [`symbios_avatar::anim`]: the gait pattern, the stride scaled to the legs
//! that take it, the footing solve, the gaze chain and the blink timing. This
//! module ticks a cycle and writes the results onto Bevy components. That
//! division is the same one the rest of the crate keeps, and for the same
//! reason: a walk that reads wrong here and right in the software renderer is
//! this crate's fault, and one that reads wrong in both is the engine's — a
//! distinction that stops being available the moment this file starts having
//! opinions about how a leg swings.
//!
//! ## Why a window rather than more keys
//!
//! Before this, the viewer's motion was three CLI flags and a held `W`. That is
//! enough to say "walk" and nothing else: it cannot hold a gait at one point in
//! its cycle, cannot slow a cadence to look at a foot plant, cannot compare a
//! trot against a wave on the same body, and cannot aim a gaze anywhere except
//! wherever the clock had swung the target when the shutter opened. All four
//! are things somebody judging a walk actually needs, and none of them is worth
//! a flag.
//!
//! Unlike a rebuild, none of this costs anything: a pose is a few dozen
//! quaternions — a blink included, since symbios-avatar#118 gave the four lids
//! joints of their own.

use bevy::prelude::*;
use symbios_avatar::Heading;
use symbios_avatar::anim::{
    GazeConfig, Idle, IdleConfig, Speed, Target, contacts_during, gaze, gesture,
};
use symbios_avatar::{
    Blink, ClipLibrary, Expression, FootingConfig, Gait, Ground, Inertializer, Leap, Pose, Rig,
    Stride, Swim, Talk, Viseme, Walk, Walked, Zone,
};

use crate::spawn::{AvatarBody, AvatarClosure, AvatarPose};

/// How long a procedural gesture takes, in seconds.
///
/// A second and a half, which is a greeting: long enough for three waves to
/// read as waves and short enough that a body is not still doing it when the
/// conversation has moved on. The engine's gestures are written in normalised
/// time, so this is the only place the real duration is decided.
const GESTURE_TIME: f32 = 1.5;

/// How wide the motion window opens, in points.
#[cfg(feature = "editor")]
const WINDOW_WIDTH: f32 = 260.0;

/// Which pattern the legs move in.
///
/// All five come from [`Gait`]; naming them here is only so a picker can offer
/// them. `Natural` is what a body walks unasked — a trot on four legs, a wave
/// on anything else — and the others are worth having because a gait that
/// looks right at the pattern the body chose can still be wrong at another.
///
/// `Running` arrived with symbios-avatar#186 and is the one that is not a walk:
/// it has a moment with nothing on the ground. #15 was filed here as a missing
/// viewer flag and turned out to be a missing gait — there was no run to select
/// — so this is that flag, finally pointing at something.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum GaitKind {
    /// Whatever suits the number of legs.
    #[default]
    Natural,
    /// Contacts lifting one after another.
    Wave,
    /// Diagonal pairs together.
    Trot,
    /// The same pattern with a flight phase — a run rather than a walk.
    Running,
    /// Every contact down, always.
    Standing,
}

impl GaitKind {
    /// Every kind, in picker order.
    pub const ALL: [GaitKind; 5] = [
        GaitKind::Natural,
        GaitKind::Wave,
        GaitKind::Trot,
        GaitKind::Running,
        GaitKind::Standing,
    ];

    /// The name to show this by.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            GaitKind::Natural => "natural",
            GaitKind::Wave => "wave",
            GaitKind::Trot => "trot",
            GaitKind::Running => "running",
            GaitKind::Standing => "standing",
        }
    }

    /// The kind that goes by this name, if any.
    ///
    /// The inverse of [`Self::label`], so a command line can reach the picker's
    /// own set. Until #15 the pattern was selectable **only** through the motion
    /// window, and `--shot` never opens a window — so of the four gaits below,
    /// a captured frame could show one.
    #[must_use]
    pub fn named(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.label() == name)
    }

    /// The gait itself, for a body.
    #[must_use]
    pub fn of(self, rig: &symbios_avatar::Rig) -> Gait {
        match self {
            GaitKind::Natural => Gait::natural(rig),
            GaitKind::Wave => Gait::wave(rig),
            GaitKind::Trot => Gait::trot(rig),
            GaitKind::Running => Gait::running(rig),
            GaitKind::Standing => Gait::standing(rig),
        }
    }
}

/// The baked clips a body can be asked to play.
///
/// A resource rather than a field on [`Animator`], because it is data and that
/// is a control surface. It also has to be **replaceable**: the artifact is
/// embedded only when `symbios-avatar/builtin-clips` is on, and a consumer that
/// fetches `clips.bin` over the network instead — which is what a wasm build
/// should do rather than carry 200 KiB it may never play — inserts its own.
///
/// Empty is a legitimate state and not an error. With no clips the motion window
/// offers the procedural gait and says so, which is exactly what this crate did
/// before there were any.
#[derive(Resource, Default)]
pub struct Clips(pub ClipLibrary);

impl Clips {
    /// The clips this build carries, or none if it carries none.
    #[must_use]
    pub fn builtin() -> Self {
        #[cfg(feature = "builtin-clips")]
        {
            // A parse failure here would mean the embedded artifact and this
            // build's reader disagree, which `symbios-avatar`'s own tests make a
            // test failure. Falling back to empty rather than panicking keeps
            // that from taking a viewer down.
            Self(ClipLibrary::builtin().unwrap_or_default())
        }
        #[cfg(not(feature = "builtin-clips"))]
        {
            Self::default()
        }
    }
}

/// A transition in progress on one body, and the two frames it came from.
///
/// Per body rather than a resource, because a blend is state about a *body* and
/// not about what the viewer is asking for. The two poses are the last two
/// frames of whatever was playing: [`Inertializer::start`] takes their
/// difference as the velocity to carry through, and without them a switch snaps.
#[derive(Component)]
pub struct Blending {
    /// The transition, once one has been started.
    running: Option<Inertializer>,
    /// The frame before last.
    previous: Pose,
    /// Last frame.
    current: Pose,
}

/// What every body in the world is doing.
///
/// One resource rather than a component per body: this drives a viewer, where
/// there is one subject and the question is always "what is it doing now".
#[derive(Resource)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "a control surface for independent behaviours is a set of switches; \
              grouping them into an enum would claim they are mutually exclusive"
)]
pub struct Animator {
    /// Whether the window draws.
    pub open: bool,
    /// Whether the legs are moving.
    pub walking: bool,
    /// Which pattern they move in.
    pub gait: GaitKind,
    /// Cycles per second.
    pub cadence: f32,
    /// Where in the cycle the body is, `0..1`.
    ///
    /// Public and writable because holding it still is the point: a gait judged
    /// at whatever phase the twelfth frame happened to land on is how a walk
    /// gets called stiff when it has only ever been seen at mid-stance.
    pub cycle: f32,
    /// Whether [`Animator::cycle`] is being scrubbed by hand instead of run.
    pub scrub: bool,
    /// A procedural gesture laid over whatever else the body is doing, and how
    /// far through it is.
    ///
    /// **Over, not instead of** — which is the whole difference between this
    /// and the swim beside it. A gesture is `Family::Expressive`: it writes the
    /// limbs it addresses and leaves the rest alone, so a body can wave while
    /// it walks. That is also why it carries its own clock rather than riding
    /// [`Animator::cycle`]: a greeting is not a cycle, it happens once and
    /// finishes, and pinning it to the gait's phase would make it play at the
    /// speed the legs happen to be going.
    pub gesture: Option<(String, f32)>,
    /// A swim to show instead of the walk, if any.
    ///
    /// **Instead of, for the same reason a leap is** (engine #244): a body
    /// cannot be mid-stride and prone in the water at once. [`Animator::cycle`]
    /// drives the stroke, so `hold` scrubs a swim exactly as it scrubs a gait,
    /// and [`Animator::cadence`] is how fast it strokes — which is the whole of
    /// the difference between treading and swimming in real time, because the
    /// engine runs both on one cycle.
    pub swim: Option<Swim>,
    /// A leap to show instead of the walk, if any.
    ///
    /// **A jump is the one motion in this crate that cannot be judged from a
    /// table** (engine #243). Its whole quality is whether the wind-up, the
    /// flight and the landing read as one movement, and the numbers say only
    /// that they meet — a body can meet at every seam and still look like three
    /// animations played in a row. So it gets a flag here, and the flag is the
    /// deliverable.
    ///
    /// [`Animator::cycle`] drives it, so `hold` scrubs a leap exactly as it
    /// scrubs a gait: `0` is the start of the wind-up and `1` is standing
    /// again.
    pub leap: Option<Leap>,
    /// How long a step is, as a multiple of what the legs would take.
    pub pace: f32,
    /// Whether the postural layer over the legs runs: the arms swinging against
    /// them, and the trunk leaning into the walk.
    ///
    /// One toggle rather than two because it exists for one purpose — taking
    /// the posture off to look at what the legs alone are doing — and because
    /// what it hides is one answer to one question. A body with neither is the
    /// mannequin #102 described.
    pub posture: bool,
    /// Whether the feet are solved onto the ground.
    pub footing: bool,
    /// Whether the eyes blink.
    pub blinking: bool,
    /// How shut the lids are held when they are not blinking.
    pub closure: f32,
    /// Whether the jaw talks.
    pub talking: bool,
    /// The pivot angle the jaw is held at when it is not talking, in radians.
    ///
    /// The still-frame control, for the same reason [`Animator::closure`]
    /// exists: speech is stochastic, so a captured frame almost never catches
    /// a syllable at its peak, and judging the mandible region's deformation
    /// needs the jaw held somewhere.
    pub opening: f32,
    /// Whether the gaze follows a target circling the body.
    pub tracking: bool,
    /// Radians per second that target travels.
    pub gaze_speed: f32,
    /// Where the target sits when it is not circling, in radians.
    pub gaze_angle: f32,
    /// Furthest the whole chain may turn from facing forward, in radians.
    pub gaze_limit: f32,
    /// Which baked clip is playing, as an index into [`Clips`].
    ///
    /// `None` is the procedural gait alone, which is what this crate did before
    /// there were clips and is one half of the comparison #141 exists to make.
    pub clip: Option<usize>,
    /// Whether the clip plays **over** the gait rather than instead of it.
    ///
    /// The third answer to the locomotion question, and the one that cannot be
    /// seen without a control for it: [`symbios_avatar::PoseClip::apply`] writes
    /// only the joints its own tracks name and leaves the rest alone, so a
    /// gesture baked from the upper body can ride a procedural walk. Legs from
    /// the engine, arms from the library.
    pub layered: bool,
    /// Whether a clip's horizontal root travel is taken out.
    ///
    /// **On by default, and the comparison is not honest without it.** A baked
    /// `Walk` carries its root about a stride forward and a looping clip wraps,
    /// so played as baked the body walks off and snaps back once a cycle while
    /// the procedural gait stays where it is. Zeroing `x` and `z` puts them on
    /// the same footing; the vertical bob is **kept**, because that is the
    /// weight the procedural gait has to be judged against and throwing it away
    /// would rig the comparison.
    pub in_place: bool,
    /// How steeply the ground rises toward `+z`, as a rise over run — the hill
    /// the body walks up or down.
    ///
    /// The viewer's floor tilts with it. A clip's ankle angles are fixed at bake
    /// time and a slope changes what they should be, so this is where an
    /// imported walk is asked the question a procedural one answers by solving.
    ///
    /// `+z` because that is the way the body faces: the engine's forward is
    /// `+z` and `Stride::for_body` strides down it (#251).
    pub grade: f32,
    /// How steeply the ground rises toward `+x`, as a rise over run — the hill
    /// the body stands ACROSS rather than climbs.
    ///
    /// A separate question from [`Self::grade`] rather than the same one turned
    /// sideways, which is why it is a second slider and not a heading: a gait
    /// answers a grade with its stride and its crouch, and a camber with its
    /// ankles and the width of its stance. Together the two reach every plane
    /// through the origin, so any slope in 3D can be put under the body (#252).
    pub camber: f32,
    /// How fast the body is turning, in degrees per second, positive toward its
    /// own left.
    ///
    /// **The control the turn has to be judged by eye through** (engine #241).
    /// A turn is three things a number can score — the skate, the yaw
    /// delivered, the sole clearance, all of which `examples/walkaudit` reads —
    /// and one it cannot: whether the body looks like it is turning or like it
    /// is being carried round a corner. That is the differential stride, the
    /// bank and the head lead composing, and the only instrument for it is this
    /// one.
    ///
    /// The gait is what turns; the ground is not. The viewer's floor is a plane
    /// through the body's own frame, so a turn on a grade shows the body
    /// carrying its own hill round with it. Judge a turn on the flat, and a
    /// slope with this at zero.
    pub turn: f32,
    /// Which way the body TRAVELS, in degrees off the way it faces: 0 is
    /// forward, 180 is backwards, +90 strafes to its own left.
    ///
    /// **A heading rather than a mode** (engine #242). The whole point of the
    /// engine expressing this as one angle is that a diagonal is a stride in
    /// its own right, so the slider can be swung continuously and nothing pops
    /// — which is the acceptance the issue was drafted with, and the one thing
    /// no table can settle. Sweep it and watch the foot roll fade out at 90
    /// degrees and come back inverted past it.
    pub heading: f32,
    /// How long a transition between sources takes, in seconds. Zero snaps.
    pub blend: f32,
    /// How far the footing solve had to move the feet on the last frame, in
    /// metres.
    ///
    /// A readout rather than a control, and the number the locomotion question
    /// should be settled on rather than on taste: a pose whose feet already land
    /// where the ground is needs no correction, and one whose do not is being
    /// held together by the solve.
    pub lift: f32,
    /// How many contacts the solve could not reach on the last frame.
    pub straining: usize,
    /// The face the body rests in, as picked in the panel.
    ///
    /// The target. What is actually showing eases toward it in EXPRESSION
    /// space through [`Expression::toward`] — the engine's own contract for
    /// why pose-space blending is wrong lives on that method — over
    /// [`Animator::blend`] seconds, the same knob every other transition here
    /// uses.
    pub expression: Expression,
    /// A lipsync mouth shape held over the expression, if any.
    ///
    /// Speech owns the mouth (symbios-avatar#218): when this is set it writes
    /// the jaw and the corners over whatever `talk` and the expression put
    /// there, which is exactly what a viseme stream arriving from an audio
    /// pipeline would do. The panel exposes it so each shape can be judged
    /// held still.
    pub viseme: Option<Viseme>,
    /// The expression currently showing — the cursor easing toward
    /// [`Animator::expression`]. Bypass-written each frame, like `lift`.
    showing: Expression,
    /// The engine's blink timer.
    /// Whether a body with nothing else to do stands and breathes.
    ///
    /// **On by default, because a body doing nothing is the state a viewer sees
    /// longest** and the one an idle exists for (engine #246). Off is what an
    /// instrument wants when it is looking at the rest pose itself: a body that
    /// is breathing and swaying has no frame that IS the rest pose, which makes
    /// a still capture of the geometry impossible to take.
    pub idle: bool,
    /// Whether the idle is the one a body holds while someone else is talking.
    ///
    /// A listener goes stiller than a body alone in a room. The talking variant
    /// is not a flag here — it follows [`Self::talking`], because a body that is
    /// speaking is a body whose idle is the speaking one, and two switches for
    /// one fact is how they come to disagree.
    pub listening: bool,
    idler: Idle,
    blink: Blink,
    /// The engine's speech driver.
    talk: Talk,
    /// How long the body has been alive, for circling the gaze target.
    elapsed: f32,
    /// What was playing last frame, so a change can start a blend.
    was: (Option<usize>, bool, bool),
}

impl Default for Animator {
    fn default() -> Self {
        Self {
            open: true,
            walking: false,
            gait: GaitKind::default(),
            cadence: 1.1,
            cycle: 0.0,
            scrub: false,
            gesture: None,
            swim: None,
            leap: None,
            pace: 1.0,
            posture: true,
            footing: true,
            blinking: true,
            closure: 0.0,
            talking: false,
            opening: 0.0,
            tracking: true,
            // Slow enough that the head is plainly tracking rather than
            // snapping, which is the thing being judged.
            gaze_speed: 0.6,
            gaze_angle: 0.0,
            // 0.6 by owner's call (2026-08-13): the engine default reaches
            // wide enough that the scan spends most of its arc with the head
            // pinned at its own mechanical limit, which reads as searching
            // rather than glancing.
            gaze_limit: 0.6,
            clip: None,
            layered: false,
            in_place: true,
            grade: 0.0,
            camber: 0.0,
            turn: 0.0,
            heading: 0.0,
            // Short enough to be a transition rather than a dissolve, long
            // enough to see. The number worth arguing about is on #141.
            blend: 0.15,
            lift: 0.0,
            straining: 0,
            expression: Expression::NEUTRAL,
            viseme: None,
            showing: Expression::NEUTRAL,
            idle: true,
            listening: false,
            idler: Idle::seeded(0x1de),
            blink: Blink::seeded(7),
            talk: Talk::seeded(7),
            elapsed: 0.0,
            was: (None, false, false),
        }
    }
}

impl Animator {
    /// How far the body has turned since the viewer started, in radians.
    ///
    /// **The viewer draws the body in place**, so a turn shows in the legs, the
    /// bank and the head and nowhere else — which is most of what there is to
    /// judge, but not the part that says whether the feet are keeping up with
    /// the heading. Yawing the body by this puts that back: the contacts stay
    /// on their patch of floor while the body comes round over them, and a foot
    /// that is skating is then impossible to miss.
    ///
    /// Published rather than left to the caller to integrate, so the yaw drawn
    /// and the yaw walked cannot drift apart — which is #252's lesson about the
    /// floor tilt, applied before it has a chance to happen twice.
    #[must_use]
    pub fn heading(&self) -> f32 {
        self.turn.to_radians() * self.elapsed
    }

    /// Whether anything at all is moving.
    ///
    /// A still body is not written every frame — not as an optimisation, but so
    /// the viewer stays honest about what a body that is doing nothing costs.
    #[must_use]
    pub fn is_idle(&self) -> bool {
        !self.walking
            && !self.blinking
            && !self.tracking
            && !self.talking
            && self.clip.is_none()
            // An expression still easing toward its target is motion: the
            // still-body rule may only re-engage once the face has arrived.
            && self.showing == self.expression
    }

    /// What is driving the body, as a value that changes when the source does.
    ///
    /// A blend has to start on the frame the answer changes, and comparing this
    /// against last frame's is how that moment is found. Scrubbing and cadence
    /// are deliberately not in it: moving a phase slider is not a transition.
    fn source(&self) -> (Option<usize>, bool, bool) {
        (self.clip, self.layered, self.walking)
    }
}

/// Drives the body, and the window that steers it.
///
/// The window needs the `editor` feature; the driving does not, because
/// applying a pose the engine computed is this crate's job whether or not
/// anything is drawing controls for it.
#[derive(Debug, Default, Clone, Copy)]
pub struct AnimatorPlugin;

impl Plugin for AnimatorPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Animator>()
            .insert_resource(Clips::builtin())
            .add_systems(
                Update,
                // After bodies are built and destroyed, before poses are applied.
                // A body rebuilt this frame must be posed this frame, and a body
                // destroyed this frame must not be posed at all.
                drive_avatar_animation.in_set(crate::AvatarSystems::Animate),
            );
        #[cfg(feature = "editor")]
        app.add_systems(bevy_egui::EguiPrimaryContextPass, animator_panel);
    }
}

/// Writes the pose and the lid closure every body should be holding.
///
/// Three cases, and the middle one is the one that is easy to get wrong.
///
/// **Something is moving** — a walk, a blink, a circling gaze — so the pose is
/// written every frame, which is what it costs.
///
/// **Nothing is moving but something was just changed.** A gaze held at an
/// angle and a lid held half shut are both poses somebody asked for, and both
/// have to be applied — but exactly once, not sixty times a second. The signal
/// is Bevy's own change detection on the resource, which is set by the window
/// that moved the slider. Nothing below writes through the [`ResMut`] unless
/// the thing it advances is running, so this system cannot keep waking itself.
///
/// **Nothing is moving and nothing changed**, and then nothing is written at
/// all. Not an optimisation: a viewer that rewrites a resting pose every frame
/// is one that cannot say what a body doing nothing costs, which is half of
/// what this crate is for.
pub fn drive_avatar_animation(
    mut commands: Commands,
    time: Res<Time>,
    clips: Res<Clips>,
    mut animator: ResMut<Animator>,
    mut bodies: Query<(Entity, Ref<AvatarBody>, Option<&mut Blending>)>,
) {
    let asked = animator.is_changed();
    if animator.is_idle() && !asked {
        return;
    }

    let delta = time.delta_secs();
    advance(&mut animator, delta);
    if animator.tracking {
        animator.elapsed += delta;
    }
    // The cursor is bypass-written for the same reason `lift` is — it is
    // this frame's readout, not an instruction.
    let showing = ease_expression(animator.showing, animator.expression, delta, animator.blend);
    animator.bypass_change_detection().showing = showing;
    // A blink is stochastic, so a single captured frame almost never catches
    // one. Holding the lids at a chosen point is what makes the geometry path
    // checkable from a still. Either way the phase runs THROUGH the
    // expression's `closure_at` — rest + (1 − rest) · phase — because adding
    // a widened rest to a full blink leaves an eye that never shuts, which is
    // the hole the engine's guard found (symbios-avatar#217).
    let closure = showing.closure_at(if animator.blinking {
        animator.blink.advance(delta)
    } else {
        animator.closure
    });
    // Speech is a pose, not geometry: the mandible region (#152) hangs off the
    // jaw pivot, so talking costs a rotation where a blink costs a rebuild.
    let jaw_angle = if animator.talking {
        animator.talk.advance(delta)
    } else {
        animator.opening
    };

    let clip = animator
        .clip
        .and_then(|which| clips.0.clips.get(which))
        .filter(|clip| clip.duration() > 0.0);
    // A clip replaces the gait unless it is asked to layer over it. Layering is
    // the interesting case and is why this is not simply `walking && clip.is_none()`.
    let gaiting = animator.walking && (clip.is_none() || animator.layered);
    let source = animator.source();
    let switched = source != animator.was;
    animator.was = source;

    for (entity, body, blending) in &mut bodies {
        let rig = &body.avatar.rig;
        let mut pose = Pose::rest(rig);
        let mut stance = Vec::new();
        // Returned rather than kept local so the ankles can roll AFTER the
        // plant: the plant lays every sole flat and a roll applied before it is
        // simply levelled away.
        // A leap replaces the walk rather than layering over it: a body cannot
        // be mid-stride and mid-air at once, and pretending otherwise is how a
        // jump ends up with a walk cycle still running underneath it.
        let walking = travelling(rig, &animator, gaiting, &mut pose, &mut stance);
        // **A body with nothing else to do stands and breathes** (engine #246).
        // Only when nothing else is driving the legs: an idle is what a body
        // does INSTEAD of walking or leaping, not a layer over them, and its
        // weight shift moves the pelvis over one foot — which on a walking body
        // would be a gait fighting a stand.
        //
        // The whole layer goes through `Idle::drive`, which advances the
        // schedule and poses every layer in one call, for the reason
        // `Walk::drive` does: a stage a caller has to remember is a stage a
        // caller forgets, and this crate has the scars.
        let idled = (animator.idle
            && walking.is_none()
            && animator.leap.is_none()
            && animator.swim.is_none())
        .then(|| standing(rig, &mut animator, &mut pose, delta, &mut stance));

        let aimed = gesturing(rig, &animator, &mut pose);
        if let Some(clip) = clip {
            // After the gait, because `PoseClip::apply` writes only the joints
            // its own tracks name — which is what lets an imported gesture ride
            // a procedural walk.
            clip.apply(rig, &mut pose, animator.cycle * clip.duration());
            if animator.in_place {
                pose.translation.x = 0.0;
                pose.translation.z = 0.0;
            }
            // A clip does not say which feet are carrying the body, so the clip
            // is asked instead. A gait does say, and its answer is better.
            //
            // `contacts_during` and not `contacts_in`: a walking foot lifts
            // about 150 mm, so for much of its swing the height test alone calls
            // it planted, and planting a swinging foot drags it to the floor and
            // ruins the walk. It reads speed off the TRAVELLING clip, which is
            // why it takes the time rather than the pose — the pose above has
            // had its root travel taken out and a planted foot in it is sliding
            // backwards at walking pace.
            if stance.is_empty() {
                stance = contacts_during(rig, clip, animator.cycle * clip.duration());
            }
        }

        // The tail of the engine's own sequence: settle the contacts, then roll
        // the ankles, in that order (engine #253). Both used to be this file's
        // to remember and the roll was simply missing, so the viewer — the
        // place a walk is judged BY EYE — drew a gait with no heel-strike and
        // no toe-off for as long as the stage existed (#251).
        //
        // Runs only when a gait is driving: a clip carries its own ankle motion
        // and rolling on top of authored feet would fight it.
        if let Some((gait, stride)) = &walking {
            let walked = settle(rig, &animator, &mut pose, gait, stride, &stance);
            // Through `bypass_change_detection`, because these are a readout
            // and not an instruction: writing them through the `ResMut` would
            // mark the resource changed every frame and defeat the still-body
            // rule this whole system is built on.
            animator.bypass_change_detection().lift = walked.lift;
            animator.bypass_change_detection().straining = walked.straining();
        }
        // **Both gaze layers stand aside for a gesture that aims the head, and
        // only for one that does** (#30). Everything below writes the head
        // outright — `look_at` assigns a chest, neck and head rotation rather
        // than composing one — so a nod applied above arrived correct and was
        // put back level a few lines later, which is a gesture the viewer could
        // not show at all.
        //
        // Asked of the clip rather than of the gesture's name, because the
        // engine already answers it: a clip that aims the head carries a
        // `Target::Gaze` track and one that does not, does not. So a wave still
        // lets the body look around while it waves — which is what a waving
        // body does — and a nod owns the head for as long as it runs. The
        // gaze slider and the idle's glance both lose to it, and that is the
        // right way round: a gesture is something the body is doing on purpose.
        if !aimed {
            glance(rig, &mut pose, idled);
            // A target at head height, applied after the gait, because looking
            // somewhere is a turn added to whatever the spine is already doing.
            let angle = if animator.tracking {
                scanned_angle(animator.elapsed, animator.gaze_speed, animator.gaze_limit)
            } else {
                animator.gaze_angle
            };
            let head = rig
                .in_zone(Zone::Head)
                .first()
                .map_or(1.5, |&joint| rig.joints[joint].position.y);
            let target = Vec3::new(angle.sin() * 2.0, head, angle.cos() * 2.0);
            gaze::look_at(
                rig,
                &mut pose,
                target,
                &GazeConfig {
                    limit: animator.gaze_limit,
                    ..GazeConfig::default()
                },
            );
        }
        // The face, after the gaze for the same reason the gaze comes after
        // the gait: everything here is added to wherever the head already is.
        pose_face(rig, &mut pose, jaw_angle, showing, animator.viseme);
        // The blend, last, so it corrects whatever the sources produced rather
        // than being overwritten by them.
        let posed = if let Some(mut blending) = blending {
            blend_into(&mut blending, pose, switched, animator.blend, delta)
        } else {
            // A body seen for the first time has no two frames to take a
            // velocity from, so it starts settled rather than blending out of
            // nothing.
            commands.entity(entity).insert(Blending {
                running: None,
                previous: pose.clone(),
                current: pose.clone(),
            });
            pose
        };
        // **The blink, AFTER the blend, and that is deliberate.** A closure is
        // a pose now (symbios-avatar#118) rather than a rebuild of two meshes,
        // so it could ride through the inertializer with everything else — and
        // it should not. A blink is about a tenth of a second from open to shut
        // and back; smoothed by a gait blend it arrives as a slow heavy-lidded
        // droop, which reads as a body falling asleep rather than as one
        // blinking. The jaw goes in before the blend because speech shares the
        // head's own timing; a lid does not.
        let mut posed = posed;
        if let Some(eyes) = body.avatar.parts.eyes.as_ref() {
            eyes.blink(&mut posed, closure);
        }
        commands.entity(entity).insert(AvatarPose(posed));
        // Kept as the record of what the lids are holding, for anything that
        // wants to ask. It no longer drives geometry: writing one used to
        // rebuild the eye meshes, which is what a blink cost before the lids
        // had joints.
        if animator.blinking || asked || body.is_added() {
            commands.entity(entity).insert(AvatarClosure(closure));
        }
    }
}

/// The jaw's pivot: the parent of the marker chain's tip (#152).
///
/// The same identification `rig::skin::bind` uses — the two markers are the
/// only joints in a rig that carry the flag — so the joint the animator turns
/// is by construction the joint the mandible region is bound to. A quadruped
/// has no markers and gets `None`, which leaves its pose untouched.
/// Writes the face's pose layers in their contract order.
///
/// Speech's jaw first; then the resting expression, which COMPOSES its jaw
/// bias over speech (a happy body keeps its parted rest while talking) and
/// owns the brows and corners outright; then a held viseme over the mouth,
/// because speech owns it (symbios-avatar#218) — a viseme writes the jaw and
/// corners over both layers above, which is what a lipsync stream would do.
/// The lids are written by nothing here: they arrive through the closure,
/// after the blend, like every blink.
fn pose_face(
    rig: &Rig,
    pose: &mut Pose,
    jaw_angle: f32,
    showing: Expression,
    viseme: Option<Viseme>,
) {
    if let Some(pivot) = jaw_pivot(rig) {
        pose.rotations[pivot] = Quat::from_rotation_x(jaw_angle);
    }
    showing.apply(rig, pose);
    if let Some(viseme) = viseme {
        viseme.apply(rig, pose);
    }
}

/// One frame of the resting face's approach to its target, in EXPRESSION
/// space, SETTLED when close: an exponential approach never lands, and a face
/// a whisker from happy would hold the still-body rule off forever.
fn ease_expression(showing: Expression, target: Expression, delta: f32, blend: f32) -> Expression {
    let step = if blend <= 0.0 {
        1.0
    } else {
        (delta / blend).min(1.0)
    };
    let eased = showing.toward(target, step);
    let settled = (eased.brows - target.brows).abs() < 5e-3
        && (eased.corners - target.corners).abs() < 5e-3
        && (eased.jaw - target.jaw).abs() < 5e-3
        && (eased.lids - target.lids).abs() < 5e-3;
    if settled { target } else { eased }
}

fn jaw_pivot(rig: &Rig) -> Option<usize> {
    (0..rig.len()).find_map(|tip| {
        let pivot = rig.joints[tip].parent?;
        (rig.joints[tip].marker && rig.joints[pivot].marker).then_some(pivot)
    })
}

/// Where a tracked gaze points, `elapsed` seconds into its scan.
///
/// A continuous scan, not a lap (#26): the target used to circle one way
/// forever, so the head swept to its limit, snapped across as the target
/// passed behind it, and swept again. A triangle wave runs the same arc at the
/// same `speed` in both directions and reverses at the ends — the scanning
/// loop the control was always meant to be. Phase-offset by one span so it
/// starts at zero moving positive; speed 0 holds it there.
fn scanned_angle(elapsed: f32, speed: f32, limit: f32) -> f32 {
    let span = limit.clamp(0.01, std::f32::consts::PI);
    let along = (span + elapsed * speed).rem_euclid(4.0 * span);
    if along < 2.0 * span {
        along - span
    } else {
        3.0 * span - along
    }
}

/// Walks the body one frame and reports what the engine's own drive did.
///
/// **Every stage, through [`Walk::drive`]** (engine #253). This crate had the
/// sequence hand-rolled and was one of the three consumers that had simply
/// forgotten `roll_feet` — for as long as it existed, so the viewer, which is
/// the place a walk is judged BY EYE, was drawing a gait missing a stage. The
/// entry point is the fix for the class rather than for that instance: the
/// order, the ground given to both the stride and the plant, and the roll
/// landing after the settle are all its problem now.
///
/// The toggles map onto its ablation switches, so turning the posture or the
/// footing off here takes off exactly that and nothing else.
fn walk(
    rig: &symbios_avatar::Rig,
    animator: &Animator,
    pose: &mut Pose,
    stance: &mut Vec<symbios_avatar::Limb>,
) -> (Gait, Stride) {
    let gait = animator.gait.of(rig);
    let mut stride =
        Stride::for_body(rig, animator.pace).toward(rig, Heading::degrees(animator.heading));
    // A yaw RATE is per second and a stride is per stance, so the cadence joins
    // them — the body's own, recovered from the stride it is walking through
    // `Speed::of` rather than named beside it (engine #241). A turn this file
    // asserted independently of the legs would be a turn the feet were not
    // taking.
    let cadence = Speed::of(rig, &gait, &stride).cadence(rig);
    if cadence > f32::EPSILON {
        stride.yaw = animator.turn.to_radians() / cadence * gait.duty;
    }
    // Footing OFF here: this crate can layer an imported clip over the
    // procedural walk, and a clip moves the legs — so the contacts are settled
    // and the ankles rolled after that, through `Walk::settle`, further down.
    let walked = Walk {
        posture: animator.posture,
        // Only while turning, and only while the postural layer is on. A gaze
        // led down a straight path is a target the head already points at, so
        // switching it on there would cost nothing and say nothing; switching
        // it on with the posture off would put a head turn on a body that is
        // deliberately being shown as bare legs.
        gaze: (animator.posture && animator.turn != 0.0).then(GazeConfig::default),
        footing: None,
        ..Walk::at(animator.cycle)
    }
    .drive(
        rig,
        pose,
        &gait,
        &stride,
        sloping(animator.grade, animator.camber),
    );
    *stance = walked.steps.stance;
    // **Both, because the tail needs both.** `Walk::settle` rolls the ankles,
    // and since engine #241 that stage also turns each contact to face where it
    // was planted — which is a property of the stride, not of the gait. Handing
    // back only the gait left the caller rebuilding a stride and gave this
    // crate two of them to keep in step.
    (gait, stride)
}

/// Aims the head where a fidget just decided to look.
///
/// **Through the engine's own gaze layer.** [`symbios_avatar::Idled`] reports a
/// POINT for exactly this reason, so the spread down the chest, neck and head
/// and the clamp at a neck's limit all stay in one place — a head turn written
/// here would be a second answer to a question that already has one.
///
/// Applied before the tracked gaze, which is a deliberate aim and outranks a
/// glance.
fn glance(rig: &Rig, pose: &mut Pose, idled: Option<symbios_avatar::Idled>) {
    let Some(target) = idled.and_then(|idled| idled.glance) else {
        return;
    };
    gaze::look_at(
        rig,
        pose,
        target,
        &symbios_avatar::anim::idle::glance_config(),
    );
}

/// Drives one frame of a body that is standing about doing nothing.
///
/// **A body with nothing else to do stands and breathes** (engine #246), and
/// this is the state a viewer sees longest. Kept out of the main system for the
/// same reason [`walk`] is: the whole layer is one call into the engine, and
/// what lives here is only the choice of which parameter set that call gets.
///
/// Only ever reached when nothing else is driving the legs. An idle is what a
/// body does INSTEAD of walking or leaping, not a layer over them — its weight
/// shift settles the pelvis over one foot, which on a walking body would be a
/// stand fighting a gait.
///
/// The talking variant follows [`Animator::talking`] rather than having a
/// switch of its own: a body that is speaking is a body whose idle is the
/// speaking one, and two switches for one fact is how they come to disagree.
fn standing(
    rig: &Rig,
    animator: &mut Animator,
    pose: &mut Pose,
    delta: f32,
    stance: &mut Vec<symbios_avatar::Limb>,
) -> symbios_avatar::Idled {
    let config = if animator.talking {
        IdleConfig::talking()
    } else if animator.listening {
        IdleConfig::listening()
    } else {
        IdleConfig::default()
    };
    // The floor is read out before the driver is borrowed, because
    // `Idle::drive_on` takes the animator mutably and the ground comes off the
    // same struct.
    let floor = sloping(animator.grade, animator.camber);
    animator.idler.set_config(config);
    let idled = animator.idler.drive_on(rig, pose, delta, floor);
    // A standing body has every foot down, which is what the footing tail needs
    // told — the idle has already solved and planted them, but the readout the
    // panel shows is taken from this list.
    *stance = rig.ground_contacts();
    idled
}

/// Moves every clock this window runs on by `delta`.
///
/// **One cursor for the gait and the clip, deliberately.** `anim::Play` is the
/// engine's own cursor and this window already has one — the phase slider,
/// which exists so a gait can be held still at one point in its cycle. Two
/// cursors would disagree the first time somebody scrubbed, and the single
/// thing an A/B most needs is that the gait and the clip are at the same point
/// when they are compared. So `cycle` runs both, and a clip's time is
/// `cycle * duration`.
///
/// **A gesture is the exception and has its own.** A greeting happens once and
/// finishes; running it on `cycle` would loop it forever and play it at
/// whatever speed the legs happen to be going. It holds at its end rather than
/// clearing itself, so a body that has waved is left with its arm back at rest
/// and the picker still says which gesture it made.
///
/// Every write here is guarded on the thing it advances actually running, which
/// is what keeps the change-detection signal in the caller from latching on.
fn advance(animator: &mut Animator, delta: f32) {
    let running = animator.walking || animator.clip.is_some();
    if running && !animator.scrub {
        animator.cycle = (animator.cycle + delta * animator.cadence).fract();
    }
    let scrubbing = animator.scrub;
    if let Some((_, through)) = &mut animator.gesture
        && !scrubbing
    {
        *through = (*through + delta / GESTURE_TIME).min(1.0);
    }
}

/// Lays the procedural gesture over whatever the body is already doing, and
/// says whether it aimed the head.
///
/// **Over, and before the baked clip**, so the two clip forms layer in the
/// order the engine describes them: goals first, angles over them. A gesture
/// writes only the parts it addresses, which is what lets a body wave while it
/// walks — and what lets it wave at all on a body that has a hand free, and not
/// on one that has none.
///
/// **The return is what the gaze layers below need to know** (#30). They write
/// the head outright, so a gesture that aims it has to be able to say so; the
/// clip already does, by carrying a [`Target::Gaze`] track, and asking the clip
/// beats keeping a list of which gestures involve the head — a list that would
/// be wrong the moment the roster grew.
fn gesturing(rig: &Rig, animator: &Animator, pose: &mut Pose) -> bool {
    let Some((name, through)) = &animator.gesture else {
        return false;
    };
    let Some(gesture) = gesture::by_name(name) else {
        return false;
    };
    gesture.apply(rig, pose, *through);
    gesture
        .tracks
        .iter()
        .any(|track| track.target == Target::Gaze)
}

/// Drives whichever way of getting about is switched on, and says whether it
/// was the walk.
///
/// **One at a time, and that is the point of gathering them here.** A body
/// cannot be mid-stride and mid-air, or mid-stride and prone in the water, at
/// once; pretending otherwise is how a jump ends up with a walk cycle still
/// running underneath it. Written as one match over the three so that adding a
/// fourth has to say what it replaces.
fn travelling(
    rig: &Rig,
    animator: &Animator,
    gaiting: bool,
    pose: &mut Pose,
    stance: &mut Vec<symbios_avatar::Limb>,
) -> Option<(Gait, Stride)> {
    match (animator.swim, animator.leap) {
        // Nothing is added to `stance`: a swimming body has nothing on the
        // ground, and handing the footing tail a contact list is what would
        // drag its feet back down to a floor it is nowhere near.
        (Some(swim), _) => {
            Swim {
                cycle: animator.cycle,
                ..swim
            }
            .drive(rig, pose);
            None
        }
        (None, Some(leap)) => {
            leaping(rig, animator, leap, pose, stance);
            None
        }
        (None, None) => gaiting.then(|| walk(rig, animator, pose, stance)),
    }
}

/// Drives one frame of a leap, on [`Animator::cycle`]'s own clock.
///
/// The cycle runs `0..1` over the whole leap — wind-up, flight and landing —
/// so `hold` scrubs a jump exactly as it scrubs a gait, which is the only way
/// to look at one instant of it.
fn leaping(
    rig: &Rig,
    animator: &Animator,
    leap: Leap,
    pose: &mut Pose,
    stance: &mut Vec<symbios_avatar::Limb>,
) {
    let leapt = leap.drive(
        rig,
        pose,
        animator.cycle * leap.duration(rig),
        sloping(animator.grade, animator.camber),
    );
    // The footing tail runs only where the body has feet down; in flight there
    // is nothing to settle and asking would drag them back to the floor.
    if leapt.stage.is_grounded() {
        *stance = rig.ground_contacts();
    }
}

/// Settles the contacts and rolls the ankles, and records what it cost.
///
/// The tail of the engine's own drive sequence (engine #253), kept apart from
/// [`walk`] because this crate can layer an imported clip between the two — a
/// clip moves the legs, so the feet are settled and the ankles rolled after it
/// rather than before.
fn settle(
    rig: &symbios_avatar::Rig,
    animator: &Animator,
    pose: &mut Pose,
    gait: &Gait,
    stride: &Stride,
    stance: &[symbios_avatar::Limb],
) -> Walked {
    Walk {
        footing: animator.footing.then(FootingConfig::default),
        ..Walk::at(animator.cycle)
    }
    .settle(
        rig,
        pose,
        gait,
        stride,
        stance,
        sloping(animator.grade, animator.camber),
    )
}

/// Which way the sloped ground faces, for a given grade and camber.
///
/// **The one place the plane is defined, and that is the point of it.** The
/// ground the feet are solved against and the floor the viewer draws are two
/// expressions of a single surface, and they have now disagreed twice: #21
/// found the drawn tilt rotating the opposite way to the solved one, and #252
/// found it square to it, because #251 moved the solved surface from `+x` to
/// `+z` and the drawn one stayed where it was. Both times the two were kept in
/// agreement by a comment saying they had to be. A comment is not a mechanism.
///
/// Now the ground closure builds its surface from this and [`floor_tilt`]
/// rotates the drawn floor onto it, so a change of axis moves both or neither,
/// whatever the axes become.
///
/// The plane is `y = camber·x + grade·z`, whose upward normal is
/// `(-camber, 1, -grade)` normalised.
#[must_use]
pub fn ground_normal(grade: f32, camber: f32) -> Vec3 {
    Vec3::new(-camber, 1.0, -grade).normalize()
}

/// How to rotate a floor mesh lying in the world's `xz` plane so it becomes the
/// ground the feet are solved against.
///
/// Published beside [`ground_normal`] rather than left to the viewer to compose,
/// because composing it is what went wrong twice. A caller applies this and has
/// nothing to get out of step; the two expressions of the plane are now one
/// call apart instead of one convention apart.
#[must_use]
pub fn floor_tilt(grade: f32, camber: f32) -> Quat {
    Quat::from_rotation_arc(Vec3::Y, ground_normal(grade, camber))
}

/// The surface the slope controls describe — position AND normal.
///
/// **Grade runs along Z, the way the body walks**, and it ran along X until
/// #251: X is the body's lateral axis — the engine's forward is `+Z` and
/// [`Stride::for_body`] strides down it — so tilting X asked the slider's
/// question about a camber the body stood across rather than a hill it climbed.
/// Camber is now that second axis on purpose (#252), so the pair reaches every
/// plane.
///
/// Shared by the footing solve and by the stride, which since symbios-avatar
/// #221 seats its stride on whatever ground it is given: handing those two
/// different floors is exactly what leaves a swing arc at the rest ground height
/// while the plant settles onto a hill.
fn sloping(grade: f32, camber: f32) -> impl Fn(Vec3) -> Option<Ground> {
    let normal = ground_normal(grade, camber);
    move |foot: Vec3| {
        Some(Ground {
            position: Vec3::new(foot.x, foot.x * camber + foot.z * grade, foot.z),
            normal,
        })
    }
}

/// Carries one body's transition forward, and remembers this frame.
///
/// A transition starts on the frame the source changes and decays from there;
/// [`Inertializer::apply`] on a finished one returns the target unchanged, so a
/// settled body costs a clone and nothing else.
fn blend_into(
    blending: &mut Blending,
    target: Pose,
    switched: bool,
    duration: f32,
    delta: f32,
) -> Pose {
    if switched && duration > 0.0 {
        let (previous, current) = (blending.previous.clone(), blending.current.clone());
        blending.running = Some(Inertializer::start(
            &previous, &current, &target, delta, duration,
        ));
    }
    let posed = match &mut blending.running {
        Some(running) if !running.finished() => {
            running.advance(delta);
            running.apply(&target)
        }
        _ => {
            blending.running = None;
            target
        }
    };
    blending.previous = std::mem::replace(&mut blending.current, posed.clone());
    posed
}

/// The clip picker and the two switches that go with it.
///
/// `none` is the procedural gait alone, and it is the first entry rather than a
/// checkbox somewhere else because it is one of the things being chosen between
/// and not the absence of a choice.
#[cfg(feature = "editor")]
/// The face's resting layer and, held over it, a lipsync shape.
///
/// The expression combo shows "custom" when the target matches no preset —
/// nothing in the panel writes one today, but a caller may.
#[cfg(feature = "editor")]
fn face_controls(ui: &mut bevy_egui::egui::Ui, animator: &mut Animator) {
    use bevy_egui::egui;
    let expression = Expression::PRESETS
        .iter()
        .find(|(_, preset)| *preset == animator.expression)
        .map_or("custom", |(name, _)| *name);
    egui::ComboBox::from_label("expression")
        .selected_text(expression)
        .show_ui(ui, |ui| {
            for (name, preset) in Expression::PRESETS {
                ui.selectable_value(&mut animator.expression, preset, name);
            }
        });
    let viseme = animator.viseme.map_or("none", |held| {
        Viseme::NAMES
            .iter()
            .find(|(_, candidate)| *candidate == held)
            .map_or("none", |(name, _)| *name)
    });
    egui::ComboBox::from_label("viseme")
        .selected_text(viseme)
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut animator.viseme, None, "none");
            for (name, candidate) in Viseme::NAMES {
                ui.selectable_value(&mut animator.viseme, Some(candidate), name);
            }
        });
}

#[cfg(feature = "editor")]
fn clip_controls(ui: &mut bevy_egui::egui::Ui, clips: &Clips, animator: &mut Animator) {
    ui.horizontal_wrapped(|ui| {
        for (which, clip) in clips.0.clips.iter().enumerate() {
            let picked = animator.clip == Some(which);
            if ui.selectable_label(picked, &clip.name).clicked() {
                animator.clip = Some(which);
            }
        }
    });
    ui.horizontal(|ui| {
        // Layering is what makes the clip a gesture on a walking body rather
        // than the walk's replacement, so the toggle drives the gait flag too.
        let mut layered = animator.layered;
        if ui
            .toggle_value(&mut layered, "over walk")
            .on_hover_text(
                "layer the clip over the procedural walk: the clip writes only \
                 the joints its own tracks name, and the gait keeps the legs",
            )
            .changed()
        {
            animator.layered = layered;
            animator.walking = layered;
            if layered && animator.gait == GaitKind::Standing {
                animator.gait = GaitKind::Natural;
            }
        }
        ui.toggle_value(&mut animator.in_place, "in place")
            .on_hover_text(
                "remove the clip's own root travel so the body stays put — \
                 played as baked, a looping walk strides off and snaps back \
                 once a cycle",
            );
    });
}

/// The window.
///
/// A window rather than a panel, and deliberately so: the record editor claims
/// an edge of the screen because it is long and is read top to bottom, and this
/// is short, consulted in passing, and belongs somewhere the body is not.
#[cfg(feature = "editor")]
pub fn animator_panel(
    mut contexts: bevy_egui::EguiContexts,
    clips: Res<Clips>,
    mut animator: ResMut<Animator>,
    bodies: Query<&crate::spawn::AvatarBody>,
) {
    use bevy_egui::egui;

    if !animator.open {
        return;
    }
    // How many legs the subject stands on decides which gaits exist to offer:
    // `natural` IS wave on two legs and IS trot on four, and trot falls back
    // to wave off four corners — so a picker listing all of them offers four
    // labels for two behaviours, and the owner rightly could not tell them
    // apart (#27). Read before the window so the closure borrows nothing.
    let legs = bodies
        .iter()
        .next()
        .map_or(2, |body| body.avatar.rig.ground_contacts().len());
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };

    // Off to the right on the first frame, and draggable after that. The
    // record panel already owns the left edge, and a window that opened over
    // the subject would have to be moved before the subject could be looked at
    // — which is the opposite of what a control for watching a body is for.
    let opens_at = [
        ctx.content_rect().right() - WINDOW_WIDTH - 16.0,
        ctx.content_rect().top() + 16.0,
    ];
    egui::Window::new("motion")
        .default_pos(opens_at)
        .default_width(WINDOW_WIDTH)
        .show(ctx, |ui| {
            locomotion_section(ui, &clips, &mut animator, legs);
            ui.separator();
            ground_section(ui, &mut animator);
            ui.separator();
            face_section(ui, &mut animator);
            ui.separator();
            gaze_section(ui, &mut animator);
        });
}

/// What the body is doing: one choice, not a matrix of toggles.
///
/// Standing, walking, or playing a clip — the clip section offering the layer
/// over the walk. Everything below the source row appears only when it acts on
/// the chosen source, so a control that does nothing is a control not shown.
#[cfg(feature = "editor")]
fn locomotion_section(
    ui: &mut bevy_egui::egui::Ui,
    clips: &Clips,
    animator: &mut Animator,
    legs: usize,
) {
    use bevy_egui::egui;
    let clipping = animator.clip.is_some();
    let swimming = animator.swim.is_some();
    let walking = animator.walking && animator.gait != GaitKind::Standing && !swimming;
    let standing = !clipping && !walking && !swimming;

    ui.label(egui::RichText::new("locomotion").strong());
    ui.horizontal(|ui| {
        if ui.selectable_label(standing, "stand").clicked() {
            // A PLANTED stand — the standing gait names every foot a stance,
            // so the footing solve can hold them to a slope; engine #230
            // keeps the stride from hopping them.
            animator.walking = true;
            animator.gait = GaitKind::Standing;
            animator.clip = None;
            animator.layered = false;
            animator.swim = None;
        }
        if ui.selectable_label(walking && !clipping, "walk").clicked() {
            animator.walking = true;
            if animator.gait == GaitKind::Standing {
                animator.gait = GaitKind::Natural;
            }
            animator.clip = None;
            animator.layered = false;
            animator.swim = None;
        }
        // A swim replaces the walk rather than layering over it, so it belongs
        // beside the others rather than in a checkbox: a body cannot be
        // mid-stride and prone in the water at once.
        if ui.selectable_label(swimming, "swim").clicked() {
            animator.swim = (!swimming).then(|| Swim::at(animator.cycle));
            animator.clip = None;
            animator.layered = false;
        }
        ui.add_enabled_ui(!clips.0.is_empty(), |ui| {
            let label = ui
                .selectable_label(clipping, "clip")
                .on_disabled_hover_text("no baked clips in this build");
            if label.clicked() && animator.clip.is_none() {
                animator.clip = Some(0);
                animator.walking = animator.layered;
            }
        });
    });
    if clipping {
        clip_controls(ui, clips, animator);
    }
    // Two legs walk one way; only four corners have a choice to make.
    if walking && legs == 4 {
        ui.horizontal(|ui| {
            for kind in [GaitKind::Wave, GaitKind::Trot] {
                let picked = animator.gait == kind;
                if ui.selectable_label(picked, kind.label()).clicked() {
                    animator.gait = kind;
                }
            }
        });
    }
    if !standing {
        ui.add(egui::Slider::new(&mut animator.cadence, 0.05..=3.0).text("cadence /s"));
        ui.horizontal(|ui| {
            ui.add(
                egui::Slider::new(&mut animator.cycle, 0.0..=1.0)
                    .text("phase")
                    .fixed_decimals(3),
            );
            ui.toggle_value(&mut animator.scrub, "hold");
        });
    }
    if walking {
        ui.add(egui::Slider::new(&mut animator.pace, 0.0..=2.0).text("pace"));
        ui.toggle_value(&mut animator.posture, "posture");
    }
    // **The one axis a swim has**, and the whole of what there is to look at:
    // zero treads water and the top of the range is a body swimming flat out.
    // The engine reads it in metres per second and normalises by the body's own
    // length, so the same slider means the same stroke on a child and a giant.
    if let Some(swim) = &mut animator.swim {
        ui.add(egui::Slider::new(&mut swim.pace, 0.0..=2.0).text("m/s"));
        ui.toggle_value(&mut swim.carriage, "carriage");
    }

    // **The gestures sit apart from the locomotion**, because they are not one
    // of the things being chosen between: a gesture is laid over whatever the
    // body is already doing, so it is its own row rather than another entry in
    // the picker above.
    ui.separator();
    ui.label(egui::RichText::new("gesture").strong());
    ui.horizontal(|ui| {
        for name in gesture::ROSTER {
            let playing = animator
                .gesture
                .as_ref()
                .is_some_and(|(chosen, _)| chosen == name);
            if ui.selectable_label(playing, *name).clicked() {
                animator.gesture = (!playing).then(|| ((*name).to_string(), 0.0));
            }
        }
    });
    if let Some((_, through)) = &mut animator.gesture {
        ui.add(
            egui::Slider::new(through, 0.0..=1.0)
                .text("through")
                .fixed_decimals(2),
        );
    }
}

/// The ground the body meets: the footing solve, its slope, and the readout.
#[cfg(feature = "editor")]
fn ground_section(ui: &mut bevy_egui::egui::Ui, animator: &mut Animator) {
    use bevy_egui::egui;
    ui.label(egui::RichText::new("ground").strong());
    ui.horizontal(|ui| {
        ui.toggle_value(&mut animator.footing, "footing");
        // The readout the locomotion question should be settled on. A pose
        // whose feet already land where the ground is needs no correction; one
        // whose do not is being held together by the solve, and the difference
        // between an imported clip and a procedural gait shows here before it
        // shows in anybody's opinion.
        ui.label(format!(
            "lifts {:.0} mm{}",
            animator.lift * 1000.0,
            match animator.straining {
                0 => String::new(),
                n => format!(", {n} straining"),
            }
        ));
    });
    // Two axes, because a plane in 3D has two (#252): the hill the body walks
    // up and the hill it stands across. Both at once is a diagonal, which is
    // the case neither slider tests on its own.
    ui.add(
        egui::Slider::new(&mut animator.grade, -0.4..=0.4)
            .text("grade (fore-aft)")
            .fixed_decimals(2),
    );
    ui.add(
        egui::Slider::new(&mut animator.camber, -0.4..=0.4)
            .text("camber (lateral)")
            .fixed_decimals(2),
    );
    ui.horizontal(|ui| {
        ui.toggle_value(&mut animator.idle, "idle")
            .on_hover_text("breath, sway, weight shift and fidgets when nothing else is driving");
        ui.add_enabled_ui(animator.idle, |ui| {
            ui.toggle_value(&mut animator.listening, "listening")
                .on_hover_text("stiller: the variant a body holds while someone else talks");
        });
    });
    ui.add(
        egui::Slider::new(&mut animator.heading, -180.0..=180.0)
            .text("heading deg (+ is left, 180 is backwards)")
            .fixed_decimals(0),
    );
    ui.add(
        egui::Slider::new(&mut animator.turn, -120.0..=120.0)
            .text("turn deg/s (+ is left)")
            .fixed_decimals(0),
    );
    ui.add(
        egui::Slider::new(&mut animator.blend, 0.0..=0.6)
            .text("blend s")
            .fixed_decimals(2),
    );
}

/// Lids, speech and expression.
#[cfg(feature = "editor")]
fn face_section(ui: &mut bevy_egui::egui::Ui, animator: &mut Animator) {
    use bevy_egui::egui;
    ui.label(egui::RichText::new("face").strong());
    ui.horizontal(|ui| {
        ui.toggle_value(&mut animator.blinking, "blink");
        ui.toggle_value(&mut animator.talking, "talk");
    });
    ui.add_enabled(
        !animator.blinking,
        egui::Slider::new(&mut animator.closure, 0.0..=1.0)
            .text("closure")
            .fixed_decimals(3),
    );
    ui.add_enabled(
        !animator.talking,
        egui::Slider::new(&mut animator.opening, 0.0..=0.35)
            .text("open rad")
            .fixed_decimals(2),
    );
    face_controls(ui, animator);
}

/// Where the head looks: the scanning loop, or an angle held by hand.
#[cfg(feature = "editor")]
fn gaze_section(ui: &mut bevy_egui::egui::Ui, animator: &mut Animator) {
    use bevy_egui::egui;
    ui.label(egui::RichText::new("gaze").strong());
    ui.toggle_value(&mut animator.tracking, "scan");
    ui.add_enabled(
        !animator.tracking,
        egui::Slider::new(
            &mut animator.gaze_angle,
            -std::f32::consts::PI..=std::f32::consts::PI,
        )
        .text("gaze")
        .fixed_decimals(2),
    );
    ui.add_enabled(
        animator.tracking,
        egui::Slider::new(&mut animator.gaze_speed, 0.0..=2.0).text("gaze /s"),
    );
    ui.add(egui::Slider::new(&mut animator.gaze_limit, 0.0..=2.5).text("gaze limit"));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spawn::{SpawnAvatar, build_requested_avatars};

    #[test]
    fn the_scan_walks_the_same_arc_at_the_same_speed_both_ways() {
        // The owner's ask verbatim: same path, same speed, both directions, a
        // continuous loop (#26). Sampled densely over two full periods: the
        // angle never leaves ±span, starts at zero, and away from the two
        // turnarounds its rate is exactly the speed asked for — in BOTH signs.
        // Two full periods at these numbers: 4·span/speed ≈ 5.7 s a period,
        // 0.005 s a sample.
        const SAMPLES: u16 = 2400;
        let (span, speed, step) = (1.0f32, 0.7f32, 0.005f32);
        let mut last = scanned_angle(0.0, speed, span);
        assert!(last.abs() < 1e-5, "the scan starts at zero, not at an edge");
        let (mut fastest, mut slowest) = (0.0f32, f32::MAX);
        let (mut leftward, mut rightward) = (false, false);
        for tick in 1..SAMPLES {
            let now = scanned_angle(f32::from(tick) * step, speed, span);
            assert!(now.abs() <= span + 1e-4, "the scan left its span: {now}");
            let rate = (now - last) / step;
            // Away from the turnarounds, where one sample straddles the fold.
            if now.abs() < span - speed * step * 2.0 {
                fastest = fastest.max(rate.abs());
                slowest = slowest.min(rate.abs());
                leftward |= rate < 0.0;
                rightward |= rate > 0.0;
            }
            last = now;
        }
        assert!(leftward && rightward, "the scan must sweep both ways");
        assert!(
            (fastest - speed).abs() < 0.02 && (slowest - speed).abs() < 0.02,
            "the sweep rate wandered: {slowest}..{fastest} against {speed}"
        );
    }
    use bevy::mesh::skinning::SkinnedMeshInverseBindposes;
    use symbios_avatar::{Archetype, AvatarRecord};

    /// How many bodies were posed on the last frame.
    ///
    /// Counted by a system rather than by a query built on the world, and that
    /// is not a style choice. A `QueryState` created outside a schedule has no
    /// meaningful last-run tick, so `Changed` through one answers a question
    /// about when the query was made rather than about when the component was
    /// written — the first version of this read zero on a body that was plainly
    /// being posed. A system has a real tick, so its filter means what it says.
    #[derive(Resource, Default)]
    struct Wrote(usize);

    /// Records how many poses the frame just wrote.
    fn count_writes(mut wrote: ResMut<Wrote>, posed: Query<Entity, Changed<AvatarPose>>) {
        wrote.0 = posed.iter().count();
    }

    /// A headless app with just enough of Bevy to build and drive a body.
    fn app() -> App {
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            AssetPlugin::default(),
            bevy::mesh::MeshPlugin,
            bevy::image::ImagePlugin::default(),
        ))
        .init_asset::<StandardMaterial>()
        .init_asset::<SkinnedMeshInverseBindposes>()
        .init_resource::<Animator>()
        // Empty by default; the tests that need clips insert their own, which is
        // also the shape a consumer fetching them at run time uses.
        .init_resource::<Clips>()
        .init_resource::<Wrote>()
        .add_systems(
            Update,
            (
                build_requested_avatars,
                drive_avatar_animation,
                count_writes,
            )
                .chain(),
        );
        app.world_mut().spawn(SpawnAvatar::from(AvatarRecord::new(
            "Driven",
            Archetype::default(),
        )));
        app.update();
        app
    }

    #[test]
    fn a_held_opening_turns_the_jaw_and_only_the_jaw() {
        // The still-frame path: `opening` is to the jaw what `closure` is to
        // the lids, and it must land in the written pose as a local rotation
        // on the pivot — the joint the mandible region (#152) is bound to.
        let mut app = app();
        {
            let mut animator = app.world_mut().resource_mut::<Animator>();
            animator.talking = false;
            animator.opening = 0.25;
        }
        app.update();
        let mut bodies = app.world_mut().query::<(&AvatarBody, &AvatarPose)>();
        let (body, pose) = bodies.single(app.world()).expect("a driven body");
        let rig = &body.avatar.rig;
        let pivot = jaw_pivot(rig).expect("a humanoid has a jaw");
        let (axis, angle) = pose.0.rotations[pivot].to_axis_angle();
        assert!(
            (angle - 0.25).abs() < 1e-3 && axis.x > 0.99,
            "the pivot holds {angle:.3} rad about {axis:?} against the 0.25 asked for"
        );
        let head = *rig.in_zone(Zone::Head).first().expect("a head");
        assert!(
            pose.0.rotations[head].to_axis_angle().1.abs() < 0.35,
            "the held opening leaked a whole-head rotation"
        );
    }

    #[test]
    fn an_expression_eases_in_its_own_space_and_settles() {
        // The picker writes a TARGET; what shows eases toward it through
        // `Expression::toward` and must SETTLE — an exponential approach that
        // never lands would hold the still-body rule off forever, which is
        // the idle contract this window is built on.
        let mut app = app();
        {
            let mut animator = app.world_mut().resource_mut::<Animator>();
            animator.blinking = false;
            animator.tracking = false;
            animator.expression = Expression::HAPPY;
            animator.blend = 0.05;
        }
        for _ in 0..120 {
            app.update();
        }
        let animator = app.world().resource::<Animator>();
        assert_eq!(
            animator.showing,
            Expression::HAPPY,
            "the face never settled on its target"
        );
        assert!(animator.is_idle(), "a settled face has to re-idle the body");

        // And the settled face is IN the written pose: the corners carry the
        // smile as local z-rotations of opposite sign.
        let mut bodies = app.world_mut().query::<(&AvatarBody, &AvatarPose)>();
        let (body, pose) = bodies.single(app.world()).expect("a driven body");
        let rig = &body.avatar.rig;
        let corners: Vec<usize> = (0..rig.len())
            .filter(|&joint| {
                rig.joints[joint].marker
                    && rig.joints[joint].node.is_some()
                    && rig.joints[joint].position.x != 0.0
                    && rig.joints[joint].parent.is_some_and(|parent| {
                        !rig.joints[parent].marker
                            && rig.joints[joint].position.y < rig.joints[parent].position.y
                    })
            })
            .collect();
        assert_eq!(corners.len(), 2, "a humanoid carries two mouth corners");
        for &corner in &corners {
            let (axis, angle) = pose.0.rotations[corner].to_axis_angle();
            assert!(
                angle > 0.2 && axis.z.abs() > 0.99,
                "a settled HAPPY left a corner at {angle:.3} rad about {axis:?}"
            );
        }
    }

    #[test]
    fn the_lids_rest_where_the_expression_says_and_a_blink_still_shuts() {
        // The closure path composes through `closure_at`, never by addition
        // (symbios-avatar#217's hole): at rest the lids sit at the
        // expression's own bias — negative for SURPRISED's widened eyes — and
        // a full manual closure still reads 1.0 through the same path.
        let mut app = app();
        {
            let mut animator = app.world_mut().resource_mut::<Animator>();
            animator.blinking = false;
            animator.tracking = false;
            animator.closure = 0.0;
            animator.expression = Expression::SURPRISED;
            animator.blend = 0.0;
        }
        app.update();
        app.update();
        let mut closures = app.world_mut().query::<&AvatarClosure>();
        let held = closures.single(app.world()).expect("a driven body").0;
        assert!(
            (held - Expression::SURPRISED.closure()).abs() < 1e-3,
            "surprised rests its lids at {held:.3} against the expression's own bias"
        );
        {
            let mut animator = app.world_mut().resource_mut::<Animator>();
            animator.closure = 1.0;
        }
        app.update();
        let held = closures.single(app.world()).expect("a driven body").0;
        assert!(
            (held - 1.0).abs() < 1e-4,
            "a full closure reads {held:.3} through the widened rest — the compositor is adding"
        );
    }

    #[test]
    fn a_held_viseme_owns_the_mouth_over_talk_and_expression() {
        // Speech owns the mouth (symbios-avatar#218): a held `aa` writes the
        // jaw over both the manual opening and the expression's parted rest,
        // at the engine's own full conversational open.
        let mut app = app();
        {
            let mut animator = app.world_mut().resource_mut::<Animator>();
            animator.blinking = false;
            animator.tracking = false;
            animator.talking = false;
            animator.opening = 0.02;
            animator.expression = Expression::HAPPY;
            animator.viseme = Some(Viseme::Aa);
            animator.blend = 0.0;
        }
        app.update();
        app.update();
        let mut bodies = app.world_mut().query::<(&AvatarBody, &AvatarPose)>();
        let (body, pose) = bodies.single(app.world()).expect("a driven body");
        let rig = &body.avatar.rig;
        let pivot = jaw_pivot(rig).expect("a humanoid has a jaw");
        let (axis, angle) = pose.0.rotations[pivot].to_axis_angle();
        let open = symbios_avatar::TalkConfig::default().open;
        assert!(
            (angle - open).abs() < 1e-3 && axis.x > 0.99,
            "a held aa turned the pivot {angle:.3} rad against talk's own {open:.3}"
        );
    }

    #[test]
    fn talking_alone_keeps_the_body_posed() {
        // A body that is only talking is not idle: the jaw is stochastic, so
        // its pose must be written every frame, the same contract blinking has.
        let mut app = app();
        {
            let mut animator = app.world_mut().resource_mut::<Animator>();
            animator.blinking = false;
            animator.tracking = false;
            animator.walking = false;
            animator.talking = true;
        }
        app.update();
        app.update();
        assert!(
            app.world().resource::<Wrote>().0 > 0,
            "a talking body went unwritten"
        );
    }

    #[cfg(feature = "builtin-clips")]
    #[test]
    fn a_picked_clip_poses_the_body_and_the_gait_does_not() {
        // The A/B this window exists for, asserted rather than eyeballed: a clip
        // replaces the gait unless it is asked to layer, so the two must produce
        // different poses from the same phase — and the clip's must differ from
        // rest, or "playing" a clip would be indistinguishable from standing.
        let mut app = app();
        app.insert_resource(Clips::builtin());
        assert!(
            !app.world().resource::<Clips>().0.is_empty(),
            "this build carries no clips to pick"
        );

        let at = |app: &mut App| {
            app.update();
            let mut bodies = app.world_mut().query::<&AvatarPose>();
            bodies
                .iter(app.world())
                .next()
                .expect("a body was posed")
                .0
                .clone()
        };

        {
            let mut animator = app.world_mut().resource_mut::<Animator>();
            animator.walking = true;
            animator.blinking = false;
            animator.tracking = false;
            animator.scrub = true;
            animator.cycle = 0.3;
            animator.blend = 0.0;
        }
        let gaited = at(&mut app);

        {
            let mut animator = app.world_mut().resource_mut::<Animator>();
            animator.clip = Some(0);
        }
        let clipped = at(&mut app);

        let apart = |a: &Pose, b: &Pose| {
            a.rotations
                .iter()
                .zip(b.rotations.iter())
                .filter(|(x, y)| x.angle_between(**y) > 1e-3)
                .count()
        };
        assert!(
            apart(&gaited, &clipped) > 0,
            "picking a clip changed nothing about the pose"
        );
        assert!(
            apart(&clipped, &Pose::rest(&rig_of(&mut app))) > 0,
            "the clip posed the body no differently from rest"
        );
    }

    #[cfg(feature = "builtin-clips")]
    #[test]
    fn switching_source_starts_a_blend_that_advances() {
        // What #141 lists as a thing to watch — what it costs to blend out of a
        // clip — asserted at both ends. A blend that never starts is a snap, and
        // one that never finishes is a body permanently offset from what it is
        // being told to do.
        let mut app = app();
        app.insert_resource(Clips::builtin());
        {
            let mut animator = app.world_mut().resource_mut::<Animator>();
            animator.walking = true;
            animator.blinking = false;
            animator.tracking = false;
            animator.blend = 0.2;
        }
        app.update();

        app.world_mut().resource_mut::<Animator>().clip = Some(0);
        app.update();
        assert!(
            blending(&mut app),
            "switching from the gait to a clip did not start a blend"
        );

        // **Not "it finishes within N frames".** A headless `app.update()` costs
        // microseconds of wall clock and `Time` reports wall clock, so a loop of
        // any length advances the transition by almost nothing — the first
        // version of this asserted a finish and failed for that reason rather
        // than for a defect. What is this crate's to assert is the WIRING: the
        // transition moves forward on its own, and finiteness is
        // `Inertializer`'s own property and is tested where it lives.
        let started = progress(&mut app);
        for _ in 0..8 {
            app.update();
        }
        assert!(
            progress(&mut app) > started,
            "the blend was started and then never advanced"
        );

        // And a zero duration is a snap rather than a transition, which is what
        // the slider's own bottom end means.
        let mut snapping = app_with_clips();
        {
            let mut animator = snapping.world_mut().resource_mut::<Animator>();
            animator.walking = true;
            animator.blend = 0.0;
        }
        snapping.update();
        snapping.world_mut().resource_mut::<Animator>().clip = Some(0);
        snapping.update();
        assert!(
            !blending(&mut snapping),
            "a zero-second blend still started a transition"
        );
    }

    /// A headless app whose clips are the ones this build carries.
    #[cfg(feature = "builtin-clips")]
    fn app_with_clips() -> App {
        let mut app = app();
        app.insert_resource(Clips::builtin());
        {
            let mut animator = app.world_mut().resource_mut::<Animator>();
            animator.blinking = false;
            animator.tracking = false;
        }
        app
    }

    /// How far through its transition the one body is.
    #[cfg(feature = "builtin-clips")]
    fn progress(app: &mut App) -> f32 {
        let mut bodies = app.world_mut().query::<&Blending>();
        bodies
            .iter(app.world())
            .next()
            .and_then(|b| b.running.as_ref())
            .map_or(1.0, symbios_avatar::Inertializer::progress)
    }

    /// Whether the one body has a transition running.
    #[cfg(feature = "builtin-clips")]
    fn blending(app: &mut App) -> bool {
        let mut bodies = app.world_mut().query::<&Blending>();
        bodies
            .iter(app.world())
            .next()
            .and_then(|b| b.running.as_ref())
            .is_some_and(|running| !running.finished())
    }

    /// The one body's rig.
    #[cfg(feature = "builtin-clips")]
    fn rig_of(app: &mut App) -> symbios_avatar::Rig {
        let mut bodies = app.world_mut().query::<&AvatarBody>();
        bodies
            .iter(app.world())
            .next()
            .expect("a body")
            .avatar
            .rig
            .clone()
    }

    #[test]
    fn a_still_body_is_posed_once_and_then_left_alone() {
        // Not an optimisation. A viewer that rewrites a resting pose sixty
        // times a second is one that cannot tell you what a body doing nothing
        // costs, which is half of what this crate is for.
        //
        // Counted by [`count_writes`], and asserted on BOTH frames: the pose
        // should be written once — the frame somebody asked for it — and then
        // not again. A check that only asserted the silence would pass just as
        // happily on an animator that never wakes up at all.
        let mut app = app();
        {
            let mut animator = app.world_mut().resource_mut::<Animator>();
            animator.walking = false;
            animator.blinking = false;
            animator.tracking = false;
            animator.closure = 0.0;
        }
        app.update();
        assert_eq!(
            app.world().resource::<Wrote>().0,
            1,
            "the frame a still pose was asked for did not write it"
        );

        app.update();
        assert_eq!(
            app.world().resource::<Wrote>().0,
            0,
            "a body doing nothing was re-posed on the next frame"
        );
    }

    #[test]
    fn a_body_that_is_moving_is_written_every_frame() {
        // The other side of the same rule, and what makes the silence above
        // mean something.
        let mut app = app();
        {
            let mut animator = app.world_mut().resource_mut::<Animator>();
            animator.walking = true;
        }
        app.update();
        app.update();
        assert_eq!(
            app.world().resource::<Wrote>().0,
            1,
            "a walking body stopped being posed"
        );
    }

    #[test]
    fn a_held_closure_is_written_even_when_nothing_moves() {
        // The failure this guards is the one that already happened once: an
        // early return produced a frame byte-identical to the open-eyed one,
        // which reads exactly like a blink that does not work.
        let mut app = app();
        {
            let mut animator = app.world_mut().resource_mut::<Animator>();
            animator.walking = false;
            animator.blinking = false;
            animator.tracking = false;
            animator.closure = 1.0;
        }
        app.update();
        let mut query = app.world_mut().query::<&AvatarClosure>();
        let shut = query
            .iter(app.world())
            .find(|closure| closure.0 > 0.5)
            .is_some();
        assert!(shut, "a held closure was never written");
    }

    #[test]
    fn scrubbing_holds_the_phase_the_hand_put_it_at() {
        // The whole reason the phase is a public field: a gait judged at
        // whatever phase a capture landed on is a gait judged at one pose.
        let mut app = app();
        {
            let mut animator = app.world_mut().resource_mut::<Animator>();
            animator.walking = true;
            animator.scrub = true;
            animator.cycle = 0.375;
        }
        app.update();
        app.update();
        let held = app.world().resource::<Animator>().cycle;
        assert_eq!(
            held.to_bits(),
            0.375_f32.to_bits(),
            "a scrubbed phase advanced anyway"
        );
    }

    #[test]
    fn walking_advances_the_phase_and_moves_the_body() {
        let mut app = app();
        {
            let mut animator = app.world_mut().resource_mut::<Animator>();
            animator.walking = true;
            animator.cycle = 0.0;
        }
        app.update();
        assert!(
            app.world().resource::<Animator>().cycle > 0.0,
            "walking did not advance the cycle"
        );
        let rest = {
            let mut bodies = app.world_mut().query::<&AvatarBody>();
            let body = bodies.iter(app.world()).next().expect("a body");
            Pose::rest(&body.avatar.rig)
        };
        let mut query = app.world_mut().query::<&AvatarPose>();
        let posed = query.iter(app.world()).next().expect("a posed body");
        assert!(
            posed
                .0
                .rotations
                .iter()
                .zip(&rest.rotations)
                .any(|(a, b)| a.angle_between(*b) > 1e-3),
            "a walking body held its rest pose"
        );
    }

    #[test]
    fn every_gait_kind_names_a_gait_the_engine_will_build() {
        let mut app = app();
        let mut bodies = app.world_mut().query::<&AvatarBody>();
        let rig = bodies
            .iter(app.world())
            .next()
            .expect("a body")
            .avatar
            .rig
            .clone();
        for kind in GaitKind::ALL {
            let gait = kind.of(&rig);
            assert!(
                !gait.is_empty(),
                "{} drove no contacts on a two-legged body",
                kind.label()
            );
            assert_eq!(
                GaitKind::named(kind.label()),
                Some(kind),
                "{} does not answer to its own name",
                kind.label()
            );
        }
    }

    #[test]
    fn a_swim_replaces_the_walk_and_lays_the_body_down() {
        // **Engine #244.** A swim has to be watched, and the viewer is where it
        // is watched, so the flag and the picker entry are the deliverable.
        // What is asserted here is only that the wiring reaches the body: that
        // the swim drives instead of the gait, that the trunk actually lies
        // over, and that the footing tail is not handed a contact to drag the
        // body back to a floor it is nowhere near. How it READS is the eye's
        // business and the reason the flag exists at all.
        let mut app = app();
        let rig = {
            let mut bodies = app.world_mut().query::<&AvatarBody>();
            bodies
                .iter(app.world())
                .next()
                .expect("a body")
                .avatar
                .rig
                .clone()
        };
        let head = *rig
            .in_zone(Zone::Head)
            .first()
            .expect("the default body has a head");
        let root = rig
            .joints
            .iter()
            .position(|joint| joint.parent.is_none())
            .expect("a root");

        let at = |app: &mut App, pace: Option<f32>| {
            {
                let mut animator = app.world_mut().resource_mut::<Animator>();
                animator.walking = true;
                animator.swim = pace.map(|pace| Swim::at(0.0).toward(pace));
                animator.scrub = true;
                animator.cycle = 0.25;
            }
            app.update();
            let mut posed = app.world_mut().query::<&AvatarPose>();
            let pose = posed.iter(app.world()).next().expect("a pose").0.clone();
            let places = pose.forward(&rig).positions;
            places[head] - places[root]
        };

        // Standing, the head is above the root and barely ahead of it. Swimming,
        // it is out in front and the two are nearly level: the body has lain
        // down, which is the one thing about a swim that is visible from a
        // single joint.
        let upright = at(&mut app, None);
        let prone = at(&mut app, Some(1.3));
        assert!(
            upright.y > upright.z.abs(),
            "a standing body's head sat {:.2} up and {:.2} forward of its root",
            upright.y,
            upright.z,
        );
        assert!(
            prone.z > prone.y,
            "a swimming body's head sat {:.2} up and {:.2} forward of its root",
            prone.y,
            prone.z,
        );
    }

    #[test]
    fn a_leap_replaces_the_walk_and_carries_the_body_off_the_ground() {
        // **#29.** A jump has to be watched, and the viewer is where it is
        // watched — so the flag is the deliverable. What is asserted here is
        // only that the wiring reaches the body: that the leap drives instead
        // of the gait, that the root actually leaves the floor mid-flight, and
        // that it comes back. How it READS is the eye's business and the reason
        // the flag exists at all.
        let mut app = app();
        let rig = {
            let mut bodies = app.world_mut().query::<&AvatarBody>();
            bodies
                .iter(app.world())
                .next()
                .expect("a body")
                .avatar
                .rig
                .clone()
        };
        let leap = Leap::to_height(0.4);
        let root = rig
            .joints
            .iter()
            .position(|joint| joint.parent.is_none())
            .expect("a root");
        let rest = rig.joints[root].position.y;

        let at = |app: &mut App, cycle: f32| {
            {
                let mut animator = app.world_mut().resource_mut::<Animator>();
                animator.leap = Some(leap);
                animator.scrub = true;
                animator.cycle = cycle;
            }
            app.update();
            let mut posed = app.world_mut().query::<&AvatarPose>();
            let pose = posed.iter(app.world()).next().expect("a pose").0.clone();
            pose.forward(&rig).positions[root].y - rest
        };

        // Mid-flight, in the leap's own timeline rather than a guess.
        let wind_up = leap.wind_up(&rig) / leap.duration(&rig);
        let flight = leap.flight() / leap.duration(&rig);
        let apex = at(&mut app, wind_up + flight * 0.5);
        assert!(
            apex > 0.2,
            "the body barely left the ground: {apex:.3} m at the apex"
        );
        // Down at the bottom of the wind-up, and back on the floor at the end.
        assert!(
            at(&mut app, wind_up * 0.5) < -0.02,
            "no wind-up to speak of"
        );
        assert!(at(&mut app, 1.0).abs() < 0.02, "it did not come back down");
    }

    #[test]
    fn a_gesture_that_aims_the_head_keeps_it_and_one_that_does_not_lets_go() {
        // **#30, and it is two assertions rather than one** because the fix is
        // a rule about which gestures win rather than a reordering. The viewer
        // aims the head at the tail of its pipeline and does it by assignment,
        // so before this a Head Nod arrived correct from the engine — 17.2
        // degrees down, measured there across eleven bodies — and was put back
        // level three lines later. It rendered as a body standing still.
        //
        // The rule is that a clip carrying a `Target::Gaze` track owns the head
        // while it plays and nothing else does. So:
        //   a nod dips the head even with the gaze slider held elsewhere;
        //   a wave leaves the slider in charge, because a waving body should
        //   still look at the person it is waving at.
        //
        // Reintroduced by dropping the `aimed` guard: the nod reads 0.0 degrees
        // and the head sits exactly where the slider put it.
        let mut app = app();
        let rig = {
            let mut bodies = app.world_mut().query::<&AvatarBody>();
            bodies
                .iter(app.world())
                .next()
                .expect("a body")
                .avatar
                .rig
                .clone()
        };
        let head = *rig
            .in_zone(Zone::Head)
            .first()
            .expect("the default body has a head");

        // Where the head points, as a pitch below level and a yaw off forward.
        let facing = |app: &mut App, gesture: &str, through: f32| {
            {
                let mut animator = app.world_mut().resource_mut::<Animator>();
                animator.gesture = Some((gesture.to_string(), through));
                // The slider rather than the scan, so the comparison has a
                // fixed thing to be measured against.
                animator.tracking = false;
                animator.gaze_angle = 0.6;
                // Held, so the gesture stays at the phase it was asked for
                // rather than advancing out of it between updates.
                animator.scrub = true;
                // **The idle off, and that is isolation rather than
                // convenience.** A gaze track is body-relative, so a breathing,
                // swaying idle carries the nod's own frame with it and the
                // head's pitch measured against the WORLD wanders by about a
                // degree — correctly, because a nod nods with the body. This
                // test is about a third source of head motion not clobbering
                // the first, so the second is switched off and the number can
                // be the engine's exact one.
                animator.idle = false;
            }
            // Several frames, because the viewer blends between poses and the
            // first one after a switch is part of the way there. What is being
            // asserted is where the head ends up, not how fast it gets there.
            for _ in 0..40 {
                app.update();
            }
            let mut posed = app.world_mut().query::<&AvatarPose>();
            let pose = posed.iter(app.world()).next().expect("a pose").0.clone();
            let out = pose.forward(&rig).rotations[head] * symbios_avatar::rig::landmark::FORWARD;
            (
                -out.y.atan2(out.z.hypot(out.x)).to_degrees(),
                out.x.atan2(out.z).to_degrees(),
            )
        };

        let (nod_pitch, nod_yaw) = facing(&mut app, "Head Nod", 0.225);
        assert!(
            (nod_pitch - 17.2).abs() < 1.0,
            "a nod at its peak pitched the head {nod_pitch:.1} degrees down, not 17.2",
        );
        assert!(
            nod_yaw.abs() < 1.0,
            "a nod let the gaze slider yaw the head by {nod_yaw:.1} degrees",
        );

        let (wave_pitch, wave_yaw) = facing(&mut app, "Greeting", 0.5);
        assert!(
            wave_pitch.abs() < 1.0,
            "a wave pitched the head {wave_pitch:.1} degrees on its own",
        );
        assert!(
            wave_yaw > 20.0,
            "a wave should leave the gaze slider in charge; the head yawed \
             {wave_yaw:.1} degrees of the 34 it was asked for",
        );
    }

    #[test]
    fn the_viewer_can_select_a_run_and_every_other_gait_is_still_a_walk() {
        // **This test used to assert the opposite, and that is the record worth
        // keeping.** #15 was filed here as a missing viewer flag; it turned out
        // no constructor in the engine reached below a duty of a half on two
        // legs — `wave` floored at `0.5 + DOUBLE_SUPPORT`, `trot` fell back to
        // `wave` off four legs, `standing` was 1.0 — so there was no run to
        // select and never had been. The finding went upstream as
        // symbios-avatar#186 and this held the gap until it landed.
        //
        // It now asserts the thing itself: exactly one selectable gait leaves
        // the ground, and it is the one called `running`. A humanoid's run was
        // a BAKED CLIP until this — `Jog` and `Sprint` — and epic #237 is
        // removing those, so the procedural run is the only one that survives.
        let mut app = app();
        let mut bodies = app.world_mut().query::<&AvatarBody>();
        let rig = bodies
            .iter(app.world())
            .next()
            .expect("a body")
            .avatar
            .rig
            .clone();
        assert_eq!(rig.ground_contacts().len(), 2, "the fixture is a biped");

        let running: Vec<&str> = GaitKind::ALL
            .into_iter()
            .filter(|kind| kind.of(&rig).has_flight())
            .map(GaitKind::label)
            .collect();
        assert_eq!(
            running,
            vec!["running"],
            "exactly one selectable gait should leave the ground"
        );
        assert!(
            GaitKind::named("running").is_some(),
            "a run the picker offers must be reachable by name from --gait"
        );
    }
}

#[cfg(test)]
mod slope_tests {
    use super::*;

    /// Grades and cambers to check, including the diagonals that only exist
    /// once there are two axes at all.
    const PLANES: [(f32, f32); 7] = [
        (0.0, 0.0),
        (0.3, 0.0),
        (-0.3, 0.0),
        (0.0, 0.3),
        (0.0, -0.3),
        (0.25, 0.25),
        (-0.2, 0.35),
    ];

    #[test]
    fn the_drawn_floor_stands_on_the_plane_the_feet_are_solved_against() {
        // **#252, and it is the test that was missing rather than the fix that
        // was wrong.** The ground the feet meet and the floor the viewer draws
        // are two expressions of one surface, and they have disagreed twice:
        // once turning opposite ways (#21) and once square to each other (#252,
        // after #251 moved the solved surface from +x to +z and the drawn floor
        // stayed). Both times a comment said they had to match and nothing
        // checked that they did.
        //
        // A floor mesh is a quad in the world's xz plane, so the transform that
        // tilts it carries `Y` to the plane's normal. Asserting that against
        // the normal the footing solve is handed is the whole invariant, and it
        // holds for any axes anyone adds later.
        //
        // **Honestly: this is a contract test over ONE source, not a
        // cross-check of two independent derivations** — `sloping` and
        // [`floor_tilt`] both read [`ground_normal`], so it cannot fail while
        // that stays true. That is the fix rather than a weakness in the test:
        // the protection is that there is one definition and the viewer applies
        // it instead of composing its own. What this pins is the PAIRING, so a
        // future axis added to one and forgotten in the other is caught here
        // rather than in somebody's eyes.
        for (grade, camber) in PLANES {
            let ground = sloping(grade, camber);
            let tilt = floor_tilt(grade, camber);

            let solved = ground(Vec3::ZERO).expect("a surface").normal;
            let drawn = tilt * Vec3::Y;
            assert!(
                drawn.distance(solved) < 1e-5,
                "grade {grade} camber {camber}: the floor faces {drawn} and the solve {solved}"
            );

            // **And the floor's own POINTS must land on the solved surface**,
            // which facing the same way does not imply: a surface whose height
            // is sampled from the wrong axes can carry a perfectly consistent
            // normal, and the first version of this test passed with the axes
            // swapped for exactly that reason. Every vertex of the floor quad
            // is a point of the world's xz plane carried through the tilt.
            for corner in [
                Vec3::new(1.0, 0.0, 1.0),
                Vec3::new(-1.0, 0.0, 1.0),
                Vec3::new(1.0, 0.0, -1.0),
                Vec3::new(-3.0, 0.0, 2.0),
            ] {
                let placed = tilt * corner;
                let beneath = ground(placed).expect("a surface").position.y;
                assert!(
                    (placed.y - beneath).abs() < 1e-5,
                    "grade {grade} camber {camber}: the floor's {corner} sits at \
                     {} where the solve puts the ground at {beneath}",
                    placed.y
                );
            }
        }
    }

    #[test]
    fn each_axis_tilts_the_ground_the_way_its_name_says() {
        // The defect #251 and #252 are both instances of: an axis that means
        // something other than its name. Grade is the hill the body WALKS up,
        // so it must change the ground's height along `+z`, the way the body
        // faces; camber is the one it stands across, along `+x`. Asserted on
        // the surface itself rather than on the normal, because a normal can be
        // right about the tilt while the height is sampled from the wrong axis.
        let ahead = Vec3::new(0.0, 0.0, 1.0);
        let aside = Vec3::new(1.0, 0.0, 0.0);

        let uphill = sloping(0.25, 0.0);
        assert!(
            (uphill(ahead).unwrap().position.y - 0.25).abs() < 1e-6,
            "a grade must raise the ground ahead of the body"
        );
        assert!(
            uphill(aside).unwrap().position.y.abs() < 1e-6,
            "a grade must leave the ground beside the body level"
        );

        let across = sloping(0.0, 0.25);
        assert!(
            across(ahead).unwrap().position.y.abs() < 1e-6,
            "a camber must leave the ground ahead of the body level"
        );
        assert!(
            (across(aside).unwrap().position.y - 0.25).abs() < 1e-6,
            "a camber must raise the ground beside the body"
        );
    }

    #[test]
    fn the_two_axes_compose_into_one_plane() {
        // What the second axis was added for: a diagonal hill, which neither
        // slider reaches alone. The surface must be the plain sum of the two,
        // and the normal must stay a unit vector pointing up rather than
        // whichever axis was applied last.
        for (grade, camber) in PLANES {
            let ground = sloping(grade, camber);
            for at in [
                Vec3::new(1.0, 0.0, 1.0),
                Vec3::new(-2.0, 0.0, 0.5),
                Vec3::new(0.7, 0.0, -1.3),
            ] {
                let surface = ground(at).expect("a surface");
                let expected = at.x * camber + at.z * grade;
                assert!(
                    (surface.position.y - expected).abs() < 1e-5,
                    "at {at}: {} against {expected}",
                    surface.position.y
                );
                assert!((surface.normal.length() - 1.0).abs() < 1e-5);
                assert!(surface.normal.y > 0.0, "the ground faced downward");
            }
        }
    }
}
