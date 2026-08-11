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
//! joints of their own. It used to be the one exception, and cost a rebuild of
//! two meshes every time it moved.

use bevy::prelude::*;
use symbios_avatar::anim::{GazeConfig, contacts_during, gait, gaze, plant_feet_of};
use symbios_avatar::{
    Blink, ClipLibrary, FootingConfig, Gait, Ground, Inertializer, Pose, Rig, Stride, Talk, Zone,
};

use crate::spawn::{AvatarBody, AvatarClosure, AvatarPose};

/// How wide the motion window opens, in points.
#[cfg(feature = "editor")]
const WINDOW_WIDTH: f32 = 260.0;

/// Which pattern the legs move in.
///
/// All four come from [`Gait`]; naming them here is only so a picker can offer
/// them. `Natural` is what a body walks unasked — a trot on four legs, a wave
/// on anything else — and the others are worth having because a gait that
/// looks right at the pattern the body chose can still be wrong at another.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum GaitKind {
    /// Whatever suits the number of legs.
    #[default]
    Natural,
    /// Contacts lifting one after another.
    Wave,
    /// Diagonal pairs together.
    Trot,
    /// Every contact down, always.
    Standing,
}

impl GaitKind {
    /// Every kind, in picker order.
    pub const ALL: [GaitKind; 4] = [
        GaitKind::Natural,
        GaitKind::Wave,
        GaitKind::Trot,
        GaitKind::Standing,
    ];

    /// The name to show this by.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            GaitKind::Natural => "natural",
            GaitKind::Wave => "wave",
            GaitKind::Trot => "trot",
            GaitKind::Standing => "standing",
        }
    }

    /// The gait itself, for a body.
    #[must_use]
    pub fn of(self, rig: &symbios_avatar::Rig) -> Gait {
        match self {
            GaitKind::Natural => Gait::natural(rig),
            GaitKind::Wave => Gait::wave(rig),
            GaitKind::Trot => Gait::trot(rig),
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
    /// How long a step is, as a multiple of what the legs would take.
    pub pace: f32,
    /// Whether the arms swing against the legs.
    pub swing_arms: bool,
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
    /// How steeply the ground rises toward `+x`, as a rise over run.
    ///
    /// The viewer's floor tilts with it. A clip's ankle angles are fixed at bake
    /// time and a slope changes what they should be, so this is where an
    /// imported walk is asked the question a procedural one answers by solving.
    pub slope: f32,
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
    /// The engine's blink timer.
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
            pace: 1.0,
            swing_arms: true,
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
            gaze_limit: GazeConfig::default().limit,
            clip: None,
            layered: false,
            in_place: true,
            slope: 0.0,
            // Short enough to be a transition rather than a dissolve, long
            // enough to see. The number worth arguing about is on #141.
            blend: 0.15,
            lift: 0.0,
            straining: 0,
            blink: Blink::seeded(7),
            talk: Talk::seeded(7),
            elapsed: 0.0,
            was: (None, false, false),
        }
    }
}

impl Animator {
    /// Whether anything at all is moving.
    ///
    /// A still body is not written every frame — not as an optimisation, but so
    /// the viewer stays honest about what a body that is doing nothing costs.
    #[must_use]
    pub fn is_idle(&self) -> bool {
        !self.walking && !self.blinking && !self.tracking && !self.talking && self.clip.is_none()
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
    // Every write below is guarded on the thing it advances actually running,
    // which is what keeps the change-detection signal above from latching on.
    //
    // **One cursor for both sources, deliberately.** `anim::Play` is the
    // engine's own cursor and this window already has one — the phase slider,
    // which exists so a gait can be held still at one point in its cycle. Two
    // cursors would disagree the first time somebody scrubbed, and the single
    // thing an A/B most needs is that the gait and the clip are at the same
    // point when they are compared. So `cycle` runs both, and a clip's time is
    // `cycle * duration`.
    let running = animator.walking || animator.clip.is_some();
    if running && !animator.scrub {
        animator.cycle = (animator.cycle + delta * animator.cadence).fract();
    }
    if animator.tracking {
        animator.elapsed += delta;
    }
    // A blink is stochastic, so a single captured frame almost never catches
    // one. Holding the lids at a chosen point is what makes the geometry path
    // checkable from a still.
    let closure = if animator.blinking {
        animator.blink.advance(delta)
    } else {
        animator.closure
    };
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
        if gaiting {
            let gait = animator.gait.of(rig);
            let stride = Stride::for_body(rig, animator.pace);
            let steps = gait::step(rig, &mut pose, &gait, &stride, animator.cycle);
            if animator.swing_arms {
                gait::swing_arms(rig, &mut pose, &gait, animator.cycle);
            }
            stance = steps.stance;
        }
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

        if animator.footing && !stance.is_empty() {
            let (lift, straining) = solve_footing(rig, &mut pose, &stance, animator.slope);
            // Through `bypass_change_detection`, because these are a readout and
            // not an instruction: writing them through the `ResMut` would mark
            // the resource changed every frame and defeat the still-body rule
            // this whole system is built on.
            animator.bypass_change_detection().lift = lift;
            animator.bypass_change_detection().straining = straining;
        }
        // A target at head height, applied after the gait, because looking
        // somewhere is a turn added to whatever the spine is already doing.
        let angle = if animator.tracking {
            animator.elapsed * animator.gaze_speed
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
        // The jaw, after the gaze for the same reason the gaze comes after the
        // gait: speech is a rotation added to wherever the head already is.
        if let Some(pivot) = jaw_pivot(rig) {
            pose.rotations[pivot] = Quat::from_rotation_x(jaw_angle);
        }
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
fn jaw_pivot(rig: &Rig) -> Option<usize> {
    (0..rig.len()).find_map(|tip| {
        let pivot = rig.joints[tip].parent?;
        (rig.joints[tip].marker && rig.joints[pivot].marker).then_some(pivot)
    })
}

/// Puts the pose's feet on the ground and reports what that took.
///
/// The lift is measured **per joint, each against itself**, before and after —
/// not as the movement of "the lowest joint of each foot", which changes
/// identity when an ankle turns and would report a heel against a toe.
///
/// A readout rather than a diagnostic: a pose whose feet already land where the
/// ground is needs no correction, and one whose do not is being held together by
/// this. That difference is the locomotion question stated as a number rather
/// than as a matter of taste.
fn solve_footing(
    rig: &symbios_avatar::Rig,
    pose: &mut Pose,
    stance: &[symbios_avatar::Limb],
    grade: f32,
) -> (f32, usize) {
    let ground = |foot: Vec3| Some(Ground::level(Vec3::new(foot.x, foot.x * grade, foot.z)));
    let before = pose.forward(rig).positions;
    let footing = plant_feet_of(rig, pose, stance, ground, &FootingConfig::default());
    let after = pose.forward(rig).positions;
    let lift = stance
        .iter()
        .flat_map(|&limb| rig.extremity_joints(limb))
        .fold(0.0f32, |worst, joint| {
            worst.max(before[joint].distance(after[joint]))
        });
    (lift, footing.straining.len())
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
fn clip_controls(ui: &mut bevy_egui::egui::Ui, clips: &Clips, animator: &mut Animator) {
    if clips.0.is_empty() {
        ui.label("no baked clips in this build");
        return;
    }
    ui.horizontal_wrapped(|ui| {
        if ui
            .selectable_label(animator.clip.is_none(), "none")
            .clicked()
        {
            animator.clip = None;
        }
        for (which, clip) in clips.0.clips.iter().enumerate() {
            let picked = animator.clip == Some(which);
            if ui.selectable_label(picked, &clip.name).clicked() {
                animator.clip = Some(which);
            }
        }
    });
    ui.add_enabled_ui(animator.clip.is_some(), |ui| {
        ui.horizontal(|ui| {
            ui.toggle_value(&mut animator.layered, "over the gait");
            ui.toggle_value(&mut animator.in_place, "in place");
        });
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
) {
    use bevy_egui::egui;

    if !animator.open {
        return;
    }
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
            ui.horizontal(|ui| {
                ui.toggle_value(&mut animator.walking, "walk");
                ui.toggle_value(&mut animator.scrub, "scrub");
            });
            ui.horizontal_wrapped(|ui| {
                for kind in GaitKind::ALL {
                    let picked = animator.gait == kind;
                    if ui.selectable_label(picked, kind.label()).clicked() {
                        animator.gait = kind;
                    }
                }
            });

            ui.separator();
            clip_controls(ui, &clips, &mut animator);
            ui.add(
                egui::Slider::new(&mut animator.cycle, 0.0..=1.0)
                    .text("phase")
                    .fixed_decimals(3),
            );
            ui.add(egui::Slider::new(&mut animator.cadence, 0.05..=3.0).text("cadence /s"));
            ui.add(egui::Slider::new(&mut animator.pace, 0.0..=2.0).text("pace"));
            ui.horizontal(|ui| {
                ui.toggle_value(&mut animator.swing_arms, "arms");
                ui.toggle_value(&mut animator.footing, "footing");
            });
            ui.add(
                egui::Slider::new(&mut animator.slope, -0.4..=0.4)
                    .text("slope")
                    .fixed_decimals(2),
            );
            ui.add(
                egui::Slider::new(&mut animator.blend, 0.0..=0.6)
                    .text("blend s")
                    .fixed_decimals(2),
            );
            // The readout the locomotion question should be settled on. A pose
            // whose feet already land where the ground is needs no correction;
            // one whose do not is being held together by the solve, and the
            // difference between an imported clip and a procedural gait shows
            // here before it shows in anybody's opinion.
            ui.label(format!(
                "footing lifts {:.0} mm{}",
                animator.lift * 1000.0,
                match animator.straining {
                    0 => String::new(),
                    n => format!(", {n} straining"),
                }
            ));

            ui.separator();
            ui.toggle_value(&mut animator.blinking, "blink");
            ui.add_enabled(
                !animator.blinking,
                egui::Slider::new(&mut animator.closure, 0.0..=1.0)
                    .text("closure")
                    .fixed_decimals(3),
            );

            ui.separator();
            ui.toggle_value(&mut animator.talking, "talk");
            ui.add_enabled(
                !animator.talking,
                egui::Slider::new(&mut animator.opening, 0.0..=0.35)
                    .text("open rad")
                    .fixed_decimals(2),
            );

            ui.separator();
            ui.toggle_value(&mut animator.tracking, "track");
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
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spawn::{SpawnAvatar, build_requested_avatars};
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
        }
    }
}
