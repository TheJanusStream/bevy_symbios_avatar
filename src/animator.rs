//! A window for steering a body, and nothing that decides how one moves.
//!
//! Every number that decides how a body *moves* comes from
//! [`symbios_avatar::anim`]: the gait pattern, the stride scaled to the legs
//! that take it, the footing solve, the gaze chain and the blink timing. Since
//! #43 that includes the decisions as well as the arithmetic — which source is
//! carrying the body, the clocks that outlive a frame, the joins between them —
//! so this module no longer ticks a cycle and poses a body. It **translates**:
//! the window's switches into one [`Inputs`], the two layers a component cannot
//! hold into closures, and the drive's answer onto Bevy components.
//!
//! That division is the same one the rest of the crate keeps, and for the same
//! reason: a walk that reads wrong here and right in the software renderer is
//! this crate's fault, and one that reads wrong in both is the engine's — a
//! distinction that stops being available the moment this file starts having
//! opinions about how a leg swings. It is also why the walk this window shows
//! is now, by construction, the walk an application draws: there is one driver
//! and both run it.
//!
//! ## A beast's states are the driver's too
//!
//! A gesture is the one motion this window still plays on a clock of its own,
//! so it can be held and scrubbed. **A beast's Sit and Lie Down are not
//! gestures in that sense** (#53, engine #377): they are states, which the
//! driver goes into at the animal's own pace, holds, and gets up from in the
//! animal's own order, and a window replaying them on its own clock would show
//! something no application draws. So the gesture row's pick of a state is
//! asked of the driver, as a consumer's `Drive::gesture` is, and so is
//! whatever gets the beast up: letting go of the pick, "Get Up", another
//! gesture, which plays once it is up, or setting it moving. The row follows
//! the driver rather than leading it - the slider reads how far down the
//! beast is, and a state the beast is not in is let go - and leads it only
//! under hold, where the slider poses the way down itself. Each body's
//! driver is asked for itself, so one built under a pick is asked it too.
//! [`Animator::gesture`] has the rules.
//!
//! ## Why a window rather than more keys
//!
//! A flag and a held key are enough to say "walk" and nothing else. They cannot
//! hold a gait at one point in its cycle, cannot slow a cadence to look at a
//! foot plant, cannot compare a trot against a wave on the same body, and cannot
//! aim a gaze anywhere except wherever the clock had swung the target when the
//! shutter opened. All four are things somebody judging a walk actually needs,
//! and none of them is worth a flag of its own.
//!
//! Unlike a rebuild, none of this costs anything: a pose is a few dozen
//! quaternions — a blink included, since the four lids have joints of their own.

use bevy::ecs::entity::EntityHashMap;
use bevy::prelude::*;
use symbios_avatar::Heading;
use symbios_avatar::anim::driver::{Carriage, DriverConfig, Inputs, Showing, WalkFlags};
use symbios_avatar::anim::{
    Clip, GazeConfig, IdleConfig, Lead, Pattern, Speed, Target, Turn, emote, gaze, gesture,
};
use symbios_avatar::{
    ClipLibrary, Expression, FootingConfig, Gait, Ground, Leap, Pose, Rig, Stride, Swim, Talk,
    Viseme, Zone,
};

use crate::driver::{AvatarDriver, Drive};
use crate::spawn::{AvatarBody, AvatarClosure, AvatarPose};

/// How long a procedural gesture takes, in seconds.
///
/// A second and a half, which is a greeting: long enough for three waves to
/// read as waves and short enough that a body is not still doing it when the
/// conversation has moved on. The engine's gestures are written in normalised
/// time, so this is the only place the real duration is decided - and it is
/// handed to the driver as its own `gesture_secs`, so a gesture a beast plays
/// once it is up takes as long as one this window plays itself (#53).
///
/// **Not a beast's states**, which keep their own time (`emote::seconds`):
/// see [`Animator::gesture`].
const GESTURE_TIME: f32 = 1.5;

/// The seed every body this window adopts is given.
///
/// **One fixed number, because a viewer has one subject.** The engine has no
/// `Default` for a driver on purpose — an idle's seed decides when its settling
/// weight shift fires and which leg it moves first, so a seed drawn from
/// somewhere the caller cannot see makes a measurement a function of how many
/// bodies were built first. A viewer wants the opposite of a room's variety:
/// the same body, seeded the same way, every run, so two captures a week apart
/// are of the same schedule. This is the number the idle was seeded with before
/// the driver owned it; the blink now shares it, which changes when a body
/// blinks and nothing a capture can see, since every capture holds the lids.
const VIEWER_SEED: u64 = 0x1de;

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
/// `Running` is the one that is not a walk: it has a moment with nothing on the
/// ground, which is what separates it from the two patterns above it.
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
    /// One of a four-legged body's patterns, whatever the speed (#370): a
    /// walk, a trot, a pace, a canter or a gallop, its cycle still the
    /// speed's. Any other body walks its own natural gait under it.
    Pattern(Pattern),
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

    /// A four-legged body's patterns, in picker order (#370).
    pub const PATTERNS: [GaitKind; 6] = [
        GaitKind::Pattern(Pattern::Walk),
        GaitKind::Pattern(Pattern::Trot),
        GaitKind::Pattern(Pattern::Pace),
        GaitKind::Pattern(Pattern::Canter(Lead::Right)),
        GaitKind::Pattern(Pattern::Gallop(Lead::Right)),
        GaitKind::Pattern(Pattern::RotaryGallop(Lead::Right)),
    ];

    /// The name to show this by.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            GaitKind::Pattern(pattern) => pattern.name(),
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
    /// own set. Without it the pattern would be selectable only through the
    /// motion window, and a capture never opens a window — so a captured frame
    /// could show one gait of the five.
    #[must_use]
    pub fn named(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .chain(Self::PATTERNS)
            .find(|kind| kind.label() == name || kind.label().replace(' ', "-") == name)
    }

    /// The gait itself, for a body.
    #[must_use]
    pub fn of(self, rig: &symbios_avatar::Rig) -> Gait {
        match self {
            // Its duty is the speed's, which the driver builds it at: this is
            // the order the feet land in, for anything that asks.
            GaitKind::Pattern(pattern) if rig.ground_contacts().len() == 4 => {
                Gait::pattern(rig, pattern, 0.5)
            }
            // A pattern is a four-legged body's, and a person left with one
            // picked - the picker is not drawn on two legs, so it cannot be
            // cleared there - walks as a person, at a person's speed.
            GaitKind::Natural | GaitKind::Pattern(_) => Gait::natural(rig),
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
    /// Cycles per second, or [`None`] for the one this speed implies.
    ///
    /// **[`None`] by default, which is the whole of what the speed axis buys
    /// here.** A cadence named beside a stride is a second answer to a question
    /// the speed already answers, and the two disagreed: this window ran the
    /// cursor at 1.1 cycles a second while drawing the stride of a body going
    /// 0.67 m/s, which is 0.81 — a body taking one step and being clocked at
    /// another. Deriving it is what stops that being expressible. `Some` is the
    /// deliberate mismatch an instrument sometimes wants: slowing the cursor to
    /// look at a foot plant is a thing to do to a walk, not a claim about it.
    pub cadence: Option<f32>,
    /// How fast the body travels, in metres a second, or [`None`] for whatever
    /// [`Animator::pace`] works out to.
    ///
    /// The one number the engine's speed axis wants — stride, duty, cadence,
    /// foot lift and the walk-run boundary all come off it. See
    /// [`Animator::pace`] for what the older control means now.
    pub speed: Option<f32>,
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
    ///
    /// **A beast's Sit and Lie Down are states, and its driver plays them**
    /// (#53, engine #377): picked, the beast goes down at its own pace
    /// (`emote::seconds`) and stays down, and the number here is how far
    /// down it is rather than this window's clock: it follows the pick's own
    /// way down and holds at one while the beast stays down, and while
    /// something gets the beast up it stays where it was. Getting up is the
    /// driver's too:
    /// letting go of the pick, picking `emote::GET_UP` (which lets go of it),
    /// or picking another gesture gets the beast up in the animal's own order
    /// (`emote::getting_up`), and the other gesture plays once it is up;
    /// walking it, swimming it or leaping it gets it up in a hurry, and the
    /// pick lets go once it is up, as it does of a state the beast would not
    /// go into because it was moving. With [`Self::scrub`] on, the number
    /// poses a state at that point of its way down instead, and whatever
    /// gets the beast up from there gets it up from that point. A body built
    /// under a state - the record editor rebuilds one on every edit - is
    /// drawn as far down as the number says from its first frame, so a
    /// sitting beast can be tuned as it sits. A person's gestures, and a
    /// beast's while it is up, run on this window's clock as they always
    /// have.
    pub gesture: Option<(String, f32)>,
    /// A swim to show instead of the walk, if any.
    ///
    /// **Instead of, for the same reason a leap is:** a body
    /// cannot be mid-stride and prone in the water at once. [`Animator::cycle`]
    /// drives the stroke, so `hold` scrubs a swim exactly as it scrubs a gait,
    /// and [`Animator::cadence`] is how fast it strokes — which is the whole of
    /// the difference between treading and swimming in real time, because the
    /// engine runs both on one cycle.
    pub swim: Option<Swim>,
    /// A leap to show instead of the walk, if any.
    ///
    /// **A jump is the one motion in this crate that cannot be judged from a
    /// table.** Its whole quality is whether the wind-up, the
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
    ///
    /// **A route to a speed now rather than a stride of its own** (#43). The
    /// engine builds a stride from a [`Speed`] and nothing else, so this is
    /// converted — `Speed::of(rig, gait, Stride::for_body(rig, pace))` — and the
    /// stride the body walks is the one that speed implies. The two
    /// derivations are not the same stride and never were: on the default body
    /// this window shows, `Stride::for_body(1.0)` is 6.2% shorter than the
    /// stride of the speed it recovers to, and at pace 1.5 it carries a full
    /// heel tuck where the speed axis puts that body below the walk-run
    /// transition with no tuck at all. This window is where a walk is judged by
    /// eye, so the stride it draws has to be the one an application draws.
    pub pace: f32,
    /// Whether the postural layer over the legs runs: the arms swinging against
    /// them, and the trunk leaning into the walk.
    ///
    /// One toggle rather than two because it exists for one purpose — taking
    /// the posture off to look at what the legs alone are doing — and because
    /// what it hides is one answer to one question. A body with neither is the
    /// mannequin a body with neither is.
    pub posture: bool,
    /// Whether the neck takes the trunk's lean back off to hold the head level.
    ///
    /// The engine's own ablation ([`symbios_avatar::Walk::head_level`], its
    /// #328), surfaced because the strips instrument exists to split the lean's
    /// contribution to a silhouette from the crane's — the head-level bargain
    /// bends the neck back by nearly twice the visible lean, and which half of
    /// the walking hunch lives where cannot be judged while the two are welded
    /// together. Off is an ablation, not a look; it does nothing while
    /// [`Animator::posture`] is off, because there is then no lean to bargain
    /// over.
    pub head_level: bool,
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
    /// there were clips and is one half of the comparison this window exists to
    /// make.
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
    /// `+z` and `Stride::for_body` strides down it.
    pub grade: f32,
    /// How steeply the ground rises toward `+x`, as a rise over run — the hill
    /// the body stands ACROSS rather than climbs.
    ///
    /// A separate question from [`Self::grade`] rather than the same one turned
    /// sideways, which is why it is a second slider and not a heading: a gait
    /// answers a grade with its stride and its crouch, and a camber with its
    /// ankles and the width of its stance. Together the two reach every plane
    /// through the origin, so any slope in 3D can be put under the body.
    pub camber: f32,
    /// How fast the body is turning, in degrees per second, positive toward its
    /// own left.
    ///
    /// **The control the turn has to be judged by eye through.** A turn is three
    /// things a number can score — the skate, the yaw
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
    /// **A heading rather than a mode.** The whole point of the engine expressing
    /// this as one angle is that a diagonal is a stride in its own right, so the
    /// slider can be swung continuously and nothing pops — the one thing no table
    /// can settle. Sweep it and watch the foot roll fade out at 90
    /// degrees and come back inverted past it.
    pub heading: f32,
    /// How long a transition between sources takes, in seconds. Zero snaps.
    pub blend: f32,
    /// Whether the footing solve could not reach a contact on the last frame.
    ///
    /// A readout rather than a control. **One flag rather than the two figures
    /// this used to show** — how far the solve moved the feet, and how many
    /// contacts it could not reach — because [`symbios_avatar::anim::Driven`]
    /// reports a bool and the settle now happens inside the drive. A body that
    /// strains occasionally is a body on hard ground; one that strains
    /// constantly is a body whose goals are wrong, and that is the whole of
    /// what this still answers. Getting the numbers back is an engine ask.
    pub strained: bool,
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
    /// Speech owns the mouth: when this is set it writes
    /// the jaw and the corners over whatever `talk` and the expression put
    /// there, which is exactly what a viseme stream arriving from an audio
    /// pipeline would do. The panel exposes it so each shape can be judged
    /// held still.
    pub viseme: Option<Viseme>,
    /// The expression currently showing — the cursor easing toward
    /// [`Animator::expression`]. Bypass-written each frame, like `lift`.
    showing: Expression,
    /// What moves the body's root.
    ///
    /// **[`Carriage::Own`] by default, which is this window's whole difference
    /// from an application.** Nothing else moves this body, so a leap flies and
    /// a walk derives every stance offset afresh rather than holding world
    /// points — a foothold ledger on a body walking on the spot would pin a
    /// treadmill's feet to the floor and tear the walk apart. The synthetic
    /// chassis in `examples/viewer.rs` is what asks for the other answer, and
    /// the difference between the two is a whole flight arc rather than a
    /// detail.
    pub carriage: Carriage,
    /// How fast the body is going UP, in metres a second.
    ///
    /// **Signed, and it is the whole of the driver's airborne state machine.**
    /// No instantaneous test can find the moment a body lands — at the apex of
    /// a jump the vertical speed is zero, which is the most airborne a body
    /// ever is — so what the driver watches for is a body that HAS been falling
    /// and has stopped. Written by the synthetic chassis; zero for a body
    /// walking on the spot, whose jumps are `leap` instead.
    pub vertical: f32,
    /// Where the body is in the world, which only [`Carriage::Chassis`] reads.
    ///
    /// Written by the synthetic chassis as it integrates a velocity; zero for a
    /// body walking on the spot, where nothing reads it.
    pub at: Vec3,
    /// How long the gait takes to believe a change of speed, in seconds.
    ///
    /// The engine's own easing, surfaced because the instrument for it is a
    /// speed STEP and nothing else: the postural terms are pure functions of the
    /// pace they are fed, so fed raw they snap, and zero here is what the walk
    /// looked like before the easing existed.
    pub pace_response: f32,
    /// Whether a body with nothing else to do stands and breathes.
    ///
    /// **On by default, because a body doing nothing is the state a viewer sees
    /// longest** and the one an idle exists for. Off is what an
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
    /// The engine's speech driver.
    ///
    /// **The one motion driver still kept here**, because the jaw is not the
    /// driver's: [`Inputs`] carries a face and a lid closure and nothing that
    /// opens a mouth, so speech is a pose this window writes over the driver's,
    /// beside the expression and the viseme. The idle, the blink and the blend
    /// all moved into [`AvatarDriver`].
    talk: Talk,
    /// How long the body has been alive, for circling the gaze target.
    elapsed: f32,
    /// What the drivers have been told of [`Self::gesture`], and what they
    /// are doing with it. Bypass-written: it is a readout.
    told: Told,
}

/// What the drivers this window steers have been told of the gesture row's
/// pick, and what they are doing with it (#53).
///
/// **A beast's state is a request, and the pick is a state of the window**,
/// so this is the edge between them: a pick is asked of a driver once, on
/// the frame it changes, as a consumer's `Drive::gesture` is.
///
/// **Once per body, not once per window.** The record editor despawns and
/// rebuilds the body on every edit, and a body built under a pick has a
/// fresh driver that was never asked it: told once for the window, a sitting
/// dog rebuilt stood up and the row let go of its sit.
#[derive(Debug, Default)]
struct Told {
    /// What each body's driver has been told, by body.
    bodies: EntityHashMap<Asked>,
    /// Whether the pick is a driver's to play rather than this window's
    /// clock's, on any body: a beast's state, or a gesture asked of a beast
    /// while its driver was playing one.
    handed: bool,
    /// Whether a driver is playing the pick itself and it is not a state: a
    /// gesture the engine cannot stop part way, so the row keeps it lit.
    busy: bool,
    /// Whether a driver was playing a gesture after the last frame: motion,
    /// for the still-body rule, held state included.
    playing: bool,
    /// How much longer to drive, in seconds: for as long as a driver plays a
    /// gesture, and once none does, for the frame after and the blend. The
    /// frame a gesture ends on stands this window's own gaze aside and starts
    /// the driver's blend, and a still body left there would keep both.
    settling: f32,
    /// Where a state was posed from the slider on the last frame, if one
    /// was: the frame that stops doing so blends into what the driver is
    /// doing, and one that gets the beast up gets it up from there.
    scrubbed: Option<f32>,
}

impl Told {
    /// A frame with no body to tell: nothing is playing, and nothing has been
    /// told, so a body built later is asked as one already here would be.
    fn no_bodies(&mut self) {
        self.bodies.clear();
        self.playing = false;
        self.busy = false;
        self.settling = 0.0;
    }
}

/// What one body's driver has been told of the gesture row's pick (#53).
#[derive(Debug, Default)]
struct Asked {
    /// The pick it was told about.
    pick: Option<String>,
    /// Whether the pick is its driver's to play, as [`Told::handed`] says.
    handed: bool,
    /// Seconds its driver has run since it was told: a gesture of the pick's
    /// name that has played longer is one from before, still getting up.
    since: f32,
}

impl Default for Animator {
    fn default() -> Self {
        Self {
            open: true,
            walking: false,
            gait: GaitKind::default(),
            cadence: None,
            speed: None,
            cycle: 0.0,
            scrub: false,
            gesture: None,
            swim: None,
            leap: None,
            pace: 1.0,
            posture: true,
            head_level: true,
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
            strained: false,
            expression: Expression::NEUTRAL,
            viseme: None,
            showing: Expression::NEUTRAL,
            carriage: Carriage::Own,
            vertical: 0.0,
            at: Vec3::ZERO,
            pace_response: DriverConfig::default().pace_response,
            idle: true,
            listening: false,
            talk: Talk::seeded(7),
            elapsed: 0.0,
            told: Told::default(),
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
    /// and the yaw walked cannot drift apart, for the same reason [`floor_tilt`]
    /// is published beside [`ground_normal`].
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
            // So is a gesture a driver is playing (#53), and the blend out of
            // one: a beast's state keeps its own clock, which writes nothing
            // here to wake on.
            && self.told.settling <= 0.0
    }

    /// How fast the body is travelling, in metres a second.
    ///
    /// [`Self::speed`] when the caller named one, and otherwise the speed
    /// [`Self::pace`] works out to through the gait it is walking — see that
    /// field for why the older control is a route to this one rather than a
    /// stride of its own.
    #[must_use]
    pub fn speed_of(&self, rig: &Rig, gait: &Gait) -> f32 {
        self.speed.unwrap_or_else(|| {
            Speed::of(rig, gait, &Stride::for_body(rig, self.pace)).metres_per_second(rig)
        })
    }

    /// How fast the cursor runs, in cycles a second.
    ///
    /// [`Self::cadence`] when the caller named one, and otherwise the cadence
    /// this speed implies — which is arithmetic rather than a second fit: a
    /// body covering one cycle length per cycle at this many metres a second
    /// takes cycles at the only rate that makes those two agree.
    ///
    /// **The legs' cadence, which is not always the speed's.** A turn works the
    /// outer foot harder than the body and holds each stance inside the hip's
    /// range ([`Turn::cadence`]), and a heading's shorter stride takes more
    /// steps for the same travel — its reach's share of the stride, at the
    /// cadence over that share. A body turning with no walk asked of it is the
    /// engine's pivot, which steps at its turn's cadence from a standstill.
    /// Straight ahead and not turning, this is the speed's own cadence to the
    /// bit.
    ///
    /// **The engine's driver's cadence where the viewer's strips are taken,
    /// and only there**: going straight, along a heading whose stride no
    /// stance bound shortens — a strafe, a back-up, the forward diagonals —
    /// and on a pivot turning past
    /// [`PIVOT_FROM`](symbios_avatar::anim::driver::PIVOT_FROM). The driver
    /// steps faster where a stance bound shortens the stride (the backward
    /// diagonals near where a split stance lines its feet up, a four-footed
    /// body stepping sideways); idles a walk asked for under its
    /// `idle_below`, or pivots it from a standstill if it turns; idles a turn
    /// under `PIVOT_FROM` with no walk; and steps the turn eased, where this
    /// takes it as asked. A cursor running at this while the driver steps
    /// otherwise holds the cycle somewhere the stride's turn is not, and the
    /// footholds drift by the difference.
    #[must_use]
    pub fn cadence_of(&self, rig: &Rig, gait: &Gait) -> f32 {
        self.cadence.unwrap_or_else(|| {
            let speed = if self.walking || self.turn == 0.0 {
                Speed::new(rig, self.speed_of(rig, gait))
            } else {
                Speed::STILL
            };
            let turning = Turn::new(self.turn.to_radians()).cadence(rig, speed);
            let reach = Heading::degrees(self.heading).reach();
            if reach >= 1.0 {
                turning
            } else {
                turning / reach.max(f32::EPSILON)
            }
        })
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
                // destroyed this frame must not be posed at all. Chained, so the
                // driver a body was adopted with exists before the frame that
                // steers it — a body built and posed in the same frame is the
                // ordinary case here, not the corner.
                (adopt_bodies, steer)
                    .chain()
                    .in_set(crate::AvatarSystems::Animate),
            );
        #[cfg(feature = "editor")]
        app.add_systems(bevy_egui::EguiPrimaryContextPass, animator_panel);
    }
}

/// Gives a body this window's driver, so the window can steer it.
///
/// **Carrying a [`Drive`] is what says a body belongs to something else**, and
/// that is the whole of how the two ways to move a body in this crate stay out
/// of each other's way: an application's chassis writes a [`Drive`] every frame
/// and [`drive_avatar_bodies`](crate::drive_avatar_bodies) runs it; this window
/// steers the bodies that have a driver and no `Drive`. A body with neither is
/// this window's to adopt — a consumer that adds [`AnimatorPlugin`] and spawns a
/// body expects it to move, and a silent nothing is the worst answer to that.
///
/// Seeded from one fixed number, because a viewer wants one repeatable subject
/// rather than a room's variety — see `VIEWER_SEED`.
#[expect(
    clippy::type_complexity,
    reason = "a Bevy query's data and its filter are one type by construction; \
              naming half of it elsewhere hides which bodies this system claims"
)]
pub fn adopt_bodies(
    mut commands: Commands,
    orphans: Query<Entity, (With<AvatarBody>, Without<AvatarDriver>, Without<Drive>)>,
) {
    for body in &orphans {
        commands
            .entity(body)
            .insert(AvatarDriver::seeded(VIEWER_SEED));
    }
}

/// Steers every body this window owns, and writes what the drive produced.
///
/// **This used to BE the driver** (#43). The state machine it ran — which
/// source is carrying the body, the airborne states, the eased pace, the idle,
/// the blink, the transition between sources — went upstream into
/// [`symbios_avatar::anim::Driver`] and reaches Bevy as [`AvatarDriver`], so
/// what is left here is a translation: the window's switches into one
/// [`Inputs`], and the two layers only this crate can supply.
///
/// **Through [`symbios_avatar::anim::Driver::drive`] directly rather than
/// through [`drive_avatar_bodies`](crate::drive_avatar_bodies)**, and that is
/// the door the component form leaves open rather than a road around it. This
/// window needs two things no component can hold: a ground closure, because the
/// grade and camber sliders put the body on a plane rather than a level floor,
/// and two overlay closures, because a baked clip, a gesture, a gaze and a face
/// all ride over the motion. Both are borrows of things a system holds.
///
/// Three cases, and the middle one is the one that is easy to get wrong.
///
/// **Something is moving** — a walk, a blink, a circling gaze — so the pose is
/// written every frame, which is what it costs. A gesture a driver is playing
/// is motion too, a beast's held state included, and so is the blend out of
/// one: none of them writes anything to the window to wake on (#53).
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
pub fn steer(
    mut commands: Commands,
    time: Res<Time>,
    clips: Res<Clips>,
    mut animator: ResMut<Animator>,
    // **Never a body that carries a `Drive`** (#42, #43). The two ways to move a
    // body in this crate are a resource and a component, and a body written by
    // both flickers between whatever each of them thinks it is doing. The
    // component wins by construction: a consumer that wrote one has said which
    // answer it wants.
    mut bodies: Query<(Entity, Ref<AvatarBody>, &mut AvatarDriver), Without<Drive>>,
) {
    let asked = animator.is_changed();
    let (resting, unasked) = pending(
        &animator,
        bodies
            .iter()
            .map(|(entity, body, _)| (entity, body.into_inner())),
    );
    if animator.is_idle() && !asked && !unasked {
        return;
    }

    let delta = time.delta_secs();
    if animator.tracking {
        animator.elapsed += delta;
    }
    advance_gesture(&mut animator, delta, resting);
    // The cursor is bypass-written for the same reason `strained` is — it is
    // this frame's readout, not an instruction.
    let showing = ease_expression(animator.showing, animator.expression, delta, animator.blend);
    animator.bypass_change_detection().showing = showing;
    // Speech is a pose, not geometry: the mandible region (#152) hangs off the
    // jaw pivot, so talking costs a rotation where a blink costs a rebuild. The
    // driver carries a face and a lid closure and nothing that opens a mouth,
    // so this stays the window's and rides in the settled layer below.
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

    // Everything below reads the window; the two readouts are written after the
    // loop, because the overlay closures borrow it for as long as a drive runs.
    let mut cursor: Option<f32> = None;
    let mut strained = false;
    // Whether any body here can make the gesture held, and whether there is
    // a body at all: see the let-go below.
    let (mut any_body, mut gesturable) = (false, false);
    // What the drivers made of the pick: see `Told`.
    let mut after = After::default();
    {
        let animator: &Animator = &animator;
        for (entity, body, mut driver) in &mut bodies {
            let rig = &body.avatar.rig;
            any_body = true;
            gesturable |= animator
                .gesture
                .as_ref()
                .is_some_and(|(name, _)| gesture::for_body(rig, name).is_some());
            let told = animator.told.bodies.get(&entity);
            let picked = animator.gait.of(rig);
            let stirred = stirred(animator, &driver, animator.speed_of(rig, &picked), gaiting);
            let mut gesturing = plan_gesture(animator, &driver, rig, told, stirred);
            // **One cursor for every body, advanced once.** `anim::Play` is the
            // engine's own cursor and this window already has one — the phase
            // slider, which exists so a gait can be held still at one point in
            // its cycle, and which the strip plan writes per sample. Two would
            // disagree the first time somebody scrubbed, and the single thing an
            // A/B most needs is that the gait and the clip are at the same point
            // when they are compared. So the driver is handed the cursor rather
            // than running one: a phase it relabelled under a change of duty
            // would move the moment a sheet is sampling, and whether it relabels
            // is the engine's own guard to keep and overlands' to measure.
            let cycle = *cursor.get_or_insert_with(|| {
                if animator.scrub {
                    animator.cycle
                } else {
                    (animator.cycle + delta * animator.cadence_of(rig, &picked)).fract()
                }
            });
            let frame = Frame {
                delta,
                cycle,
                gaiting,
                picked: &picked,
                clip,
                jaw_angle,
                showing,
                eyes: body.avatar.parts.eyes.as_ref(),
            };
            let brought = bring(animator, &mut driver, rig, &frame, &mut gesturing, told);
            // **Leaving a scrub blends into what the driver is doing**, which
            // went on under it: a state posed from the slider is not where the
            // driver's own has got to, and letting go of hold would jump there.
            // After `bring`, whose frames would take the request otherwise.
            if animator.told.scrubbed.is_some() && !gesturing.scrubbing {
                driver.blend_now();
            }
            gesturing.since += brought + delta;
            let Some(driven) = drive_body(animator, &mut driver, rig, &frame, &gesturing) else {
                continue;
            };
            // **A head this window left to a driver that did not aim it is
            // blended back.** Whether the driver's gesture aims is read before
            // its frame, and the frame one ends on - or the one a state is
            // refused on - is the one that misreads: the head stood where the
            // body had it, and the next frame's gaze would turn it in one.
            if gesturing.driver_aims && !gesturing.own_aims() && !driven.aimed {
                driver.blend_now();
            }
            after.read(animator, &driver, rig, &gesturing, entity);
            strained |= driven.strained;
            commands.entity(entity).insert(AvatarPose(driven.pose));
            // Kept as the record of what the lids are holding, for anything that
            // wants to ask. It no longer drives geometry: writing one used to
            // rebuild the eye meshes, which is what a blink cost before the lids
            // had joints.
            if animator.blinking || asked || body.is_added() {
                commands
                    .entity(entity)
                    .insert(AvatarClosure(driven.closure));
            }
        }
    }
    // **A gesture no body here can make is let go** (#370). The picker holds a
    // gesture at its end, so a dog's sit outlived the dog when a person was
    // loaded, and the person's row, which offers a person's roster, had no
    // way to clear it. Written only when there is one to clear, so it marks
    // the resource changed once rather than every frame.
    //
    // **And so is a state the beast is not in** (#53): one it got up from
    // because it was moved, or would not go into because it was moving - the
    // pick says what the body is doing, and a beast walking under a lit "Sit"
    // is not sitting. "Get Up" is let go on the frame it is asked for: it is
    // an act, not something a body goes on doing.
    let getting_up = animator
        .gesture
        .as_ref()
        .is_some_and(|(name, _)| name == emote::GET_UP);
    if any_body && (!gesturable || after.ended || getting_up) && animator.gesture.is_some() {
        animator.gesture = None;
    }
    if any_body {
        after.tell(animator.bypass_change_detection(), delta);
    } else {
        animator.bypass_change_detection().told.no_bodies();
    }
    // Through `bypass_change_detection`, because these are readouts and not
    // instructions: writing them through the `ResMut` would mark the resource
    // changed every frame and defeat the still-body rule above.
    if let Some(cycle) = cursor {
        animator.bypass_change_detection().cycle = cycle;
    }
    animator.bypass_change_detection().strained = strained;
}

/// What the drivers made of the gesture row's pick this frame, gathered over
/// the bodies and written back once (#53): see [`Told`].
#[derive(Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "readouts of independent facts about one frame, gathered body by body"
)]
struct After {
    /// What each body's driver has been told, after this frame.
    bodies: EntityHashMap<Asked>,
    /// Whether a driver is playing a gesture.
    playing: bool,
    /// Whether a driver is playing the pick itself and it is not a state.
    busy: bool,
    /// Where a state was posed from the slider, if one was.
    scrubbed: Option<f32>,
    /// Whether the pick is a state a beast is not in, and so is let go.
    ended: bool,
    /// Whether a driver finished a gesture it was handed.
    finished: bool,
    /// How far through the pick a driver is, `0..1`.
    readout: Option<f32>,
}

impl After {
    /// Reads one body's driver after its frame.
    fn read(
        &mut self,
        animator: &Animator,
        driver: &AvatarDriver,
        rig: &Rig,
        gesturing: &Gesturing,
        body: Entity,
    ) {
        let now = driver.gesture();
        self.playing |= now.is_some();
        if gesturing.scrubbing {
            self.scrubbed = gesturing.own.as_ref().map(|(_, through)| *through);
        }
        let mut asked = Asked {
            pick: animator.gesture.as_ref().map(|(name, _)| name.clone()),
            handed: gesturing.handed,
            since: gesturing.since,
        };
        if let Some((pick, _)) = &animator.gesture
            && rests(rig)
        {
            match now {
                None if state(rig, pick) && !gesturing.scrubbing => self.ended = true,
                None if gesturing.handed => {
                    self.finished = true;
                    asked.handed = false;
                }
                // A state's way down at its own pace, and anything else at the
                // pace this window gives it: held at one once it is there, as
                // the window's own clock holds a gesture at its end. **Only the
                // pick's own going down and holding**: a state of the pick's
                // name that has played longer than the pick has been asked for
                // is one from before, still getting up, and one the body is
                // being moved out of is coming back up - the slider stays
                // where it was for both, rather than reading a way down the
                // body is not on.
                Some((name, elapsed)) if name == pick && !gesturing.scrubbing => {
                    let resting = state(rig, name);
                    self.busy |= !resting;
                    if elapsed <= gesturing.since + 1.0e-3 && !(resting && gesturing.stirred) {
                        let span = emote::seconds(rig, name).unwrap_or(GESTURE_TIME);
                        self.readout = Some((elapsed / span).min(1.0));
                    }
                }
                _ => {}
            }
        }
        self.bodies.insert(body, asked);
    }

    /// Writes what was read back to the window, which has let go of any pick
    /// it had to by now, `delta` seconds after the last frame.
    fn tell(mut self, animator: &mut Animator, delta: f32) {
        // What every body has been told is the pick as it stands now: one let
        // go of was let go of for all of them.
        let pick = animator.gesture.as_ref().map(|(name, _)| name.clone());
        for asked in self.bodies.values_mut() {
            if asked.pick != pick {
                *asked = Asked {
                    pick: pick.clone(),
                    ..Asked::default()
                };
            }
        }
        // A gesture a driver played is held at its end once it is done, as
        // one this window plays is.
        let handed = self.bodies.values().any(|asked| asked.handed);
        if let Some((_, through)) = &mut animator.gesture {
            if self.finished {
                *through = 1.0;
            } else if let Some(readout) = self.readout {
                *through = readout;
            }
        }
        // At least the frame after the one a gesture ended on, and then as
        // long as the blend takes.
        let settling = if self.playing || animator.told.playing {
            animator.blend.max(f32::EPSILON)
        } else {
            (animator.told.settling - delta).max(0.0)
        };
        animator.told = Told {
            bodies: self.bodies,
            handed,
            busy: self.busy && pick.is_some(),
            playing: self.playing,
            settling,
            scrubbed: self.scrubbed,
        };
    }
}

/// One body's part in the gesture row's pick, for one frame (#53).
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent facts about one body's frame, decided once and read in several places"
)]
struct Gesturing<'a> {
    /// Whether the pick changed since this body's driver was told of it, or
    /// it has never been told: a body new to the window is.
    anew: bool,
    /// Whether the window is moving the body: see [`stirred`].
    stirred: bool,
    /// Seconds the body's driver has run since it was told the pick, as of
    /// the start of this frame: see [`Asked::since`].
    since: f32,
    /// The name asked of the body's driver this frame: a request, taken once,
    /// as [`Inputs::gesture`] is.
    ask: Option<&'a str>,
    /// The clip this window lays over the motion itself, and how far into it.
    own: Option<(Clip, f32)>,
    /// Whether the gesture the driver is playing, or the one asked of it,
    /// aims the head.
    driver_aims: bool,
    /// Whether the pick is the driver's to play, as [`Told::handed`] says.
    handed: bool,
    /// Whether `own` is a state posed from the slider.
    scrubbing: bool,
}

impl Gesturing<'_> {
    /// Whether the clip this window lays aims the head.
    fn own_aims(&self) -> bool {
        self.own.as_ref().is_some_and(|(clip, _)| aims(clip))
    }
}

/// Whether `clip` aims the head: it carries a [`Target::Gaze`] track.
fn aims(clip: &Clip) -> bool {
    clip.tracks.iter().any(|track| track.target == Target::Gaze)
}

/// Whether `name` is a state on `rig`: a beast's sit or lying down (#53),
/// the emotes that keep their own time and so the only ones
/// `emote::seconds` answers for.
fn state(rig: &Rig, name: &str) -> bool {
    emote::seconds(rig, name).is_some()
}

/// Whether `rig` has states in its roster: whether it is a beast's.
fn rests(rig: &Rig) -> bool {
    emote::ROSTER.iter().any(|name| state(rig, name))
}

/// What the window's pick is to the bodies before their frame (#53):
/// whether it is a state on any of them, which keeps its own time and not
/// this window's clock, and whether a beast among them has not been told of
/// it. A beast built under a pick has to be asked it, which writes nothing to
/// the window to wake on - and with blinking off nothing else would.
fn pending<'b>(
    animator: &Animator,
    bodies: impl Iterator<Item = (Entity, &'b AvatarBody)>,
) -> (bool, bool) {
    let Some((name, _)) = &animator.gesture else {
        return (false, false);
    };
    let (mut resting, mut unasked) = (false, false);
    for (entity, body) in bodies {
        let rig = &body.avatar.rig;
        resting |= state(rig, name);
        unasked |= rests(rig)
            && animator
                .told
                .bodies
                .get(&entity)
                .is_none_or(|told| told.pick.as_ref() != Some(name));
    }
    (resting, unasked)
}

/// What one body does with the gesture row's pick this frame (#53).
///
/// **A person's pick is this window's**, played on its own clock as it always
/// was. **A beast's states are its driver's**, so that what the window shows
/// is what a consumer's [`Drive::gesture`] gets: a state is asked of the
/// driver on the frame it is picked, and the driver goes down, holds and gets
/// up. So is anything picked while the driver is playing one - it gets the
/// beast up and plays once it is - and letting go of a state is asking for
/// [`emote::GET_UP`]. What is left to the window is a beast's gesture while it
/// is up, which is a person's case, and a state posed from the slider under
/// hold: the driver's own clip for the way down, at the slider's point - but
/// not on a body being moved, which the driver will not let rest.
///
/// `told` is what this body's driver was told before: see [`Told`].
fn plan_gesture<'a>(
    animator: &'a Animator,
    driver: &AvatarDriver,
    rig: &Rig,
    told: Option<&Asked>,
    stirred: bool,
) -> Gesturing<'a> {
    let anew = told.map(|told| told.pick.as_ref())
        != Some(animator.gesture.as_ref().map(|(name, _)| name));
    let pick = animator
        .gesture
        .as_ref()
        .map(|(name, through)| (name.as_str(), *through));
    let beast = rests(rig);
    let playing = driver.gesture().map(|(name, _)| name);
    let ask = if anew && beast {
        match pick {
            // "Get Up" among them, which gets a beast up out of a state and
            // does nothing to one that is not in one.
            Some((name, _)) if state(rig, name) || playing.is_some() => Some(name),
            None if playing.is_some_and(|name| state(rig, name)) => Some(emote::GET_UP),
            _ => None,
        }
    } else {
        None
    }
    // Nor a state of a body being moved, which the driver refuses: asked, it
    // would still stand the window's gaze aside for a frame.
    .filter(|&ask| !(stirred && state(rig, ask)));
    let handed = if anew {
        ask.is_some_and(|ask| ask != emote::GET_UP && pick.is_some_and(|(name, _)| name == ask))
    } else {
        told.is_some_and(|told| told.handed)
    };
    let scrubbing =
        beast && animator.scrub && !stirred && pick.is_some_and(|(name, _)| state(rig, name));
    let own = match pick {
        // Never quite at nought: a rest asked for none of itself poses
        // nothing, which would leave the driver's own state showing under a
        // slider that says the beast is standing.
        Some((name, through)) if scrubbing => {
            gesture::for_body(rig, name).map(|clip| (clip, through.max(f32::EPSILON)))
        }
        // A state, or anything else the driver was handed, is the driver's.
        Some((name, _)) if beast && (state(rig, name) || handed) => None,
        Some((name, through)) => gesture::for_body(rig, name).map(|clip| (clip, through)),
        None => None,
    };
    let driver_aims = playing
        .into_iter()
        .chain(ask)
        .any(|name| gesture::for_body(rig, name).is_some_and(|clip| aims(&clip)));
    Gesturing {
        anew,
        stirred,
        since: told.filter(|_| !anew).map_or(0.0, |told| told.since),
        ask,
        own,
        driver_aims,
        handed,
        scrubbing,
    }
}

/// How fast a body may be carried and still rest, in metres a second: the
/// engine's own figure for a body being moved (engine #377, private there),
/// which gets a beast up out of a state and keeps it from going into one.
const STIR: f32 = 0.1;

/// Whether the window is moving a body this frame as the engine counts it
/// (#53): carried faster than [`STIR`], shown swimming or leaping, or still
/// in the air. Read before the driver's frame, so a state is not posed from
/// the slider over a body its driver will not let rest, and the slider does
/// not read a way down the body is coming back up.
fn stirred(animator: &Animator, driver: &AvatarDriver, speed: f32, gaiting: bool) -> bool {
    (gaiting && speed.abs() >= STIR)
        || animator.swim.is_some()
        || animator.leap.is_some()
        || driver.airborne().is_some()
}

/// The step a driver is run in through frames nobody is shown, in seconds:
/// three of a display's.
///
/// Coarse because only where the frames end is kept, and that does not
/// depend on the step: a state moves on in proportion to the time, and the
/// springs cut a long frame into substeps of their own. Each is a whole
/// drive, and getting a beast up and down again unseen is up to four and a
/// half seconds of them.
const UNSEEN_STEP: f32 = 1.0 / 20.0;

/// Brings a body's driver to where the window says the body is, before its
/// frame (#53), through frames nobody is shown; answers how many seconds it
/// ran them for. Two cases, both on a frame the body is new to the pick.
///
/// **A body built under a state the window says it is part way into, or
/// down in, is seated there.** The record editor rebuilds a body on every
/// edit, so a beast that went down again each time could not be tuned while
/// it sits: a new driver is asked the state and run through as much of its
/// way down as the slider reads, and the body is drawn already there.
///
/// **A state posed from the slider and let go of for something that gets
/// the beast up rises from the slider's point.** The driver's own state went
/// on underneath, so it is down where the slider may say half way, and
/// getting it up from there dropped the body into a full sit before it rose.
/// So the driver is got up, unseen, and taken down to the slider's point,
/// still under the slider's pose; the frame's own request then gets it up
/// from part way down, which the driver does the way it was going.
fn bring<'a>(
    animator: &'a Animator,
    driver: &mut AvatarDriver,
    rig: &Rig,
    frame: &Frame<'_>,
    gesturing: &mut Gesturing<'a>,
    told: Option<&'a Asked>,
) -> f32 {
    if !gesturing.anew {
        return 0.0;
    }
    if let Some((name, through)) = &animator.gesture
        && gesturing.ask == Some(name.as_str())
        && *through > 0.0
        && driver.gesture().is_none()
        && let Some(down) = emote::seconds(rig, name)
    {
        let ran = drive_unseen(
            animator,
            driver,
            rig,
            frame,
            gesturing,
            name,
            way_into(down, *through),
        );
        // Asked already, by the first of them.
        gesturing.ask = None;
        return ran;
    }
    if let Some(at) = animator.told.scrubbed
        && !gesturing.scrubbing
        && let Some(ask) = gesturing.ask
        && let Some(was) = told
            .and_then(|told| told.pick.as_deref())
            .filter(|&was| was != ask)
        && let (Some(down), Some((_, up))) = (emote::seconds(rig, was), emote::getting_up(rig, was))
    {
        let mut own = gesture::for_body(rig, was).map(|clip| (clip, at));
        std::mem::swap(&mut gesturing.own, &mut own);
        // Up from wherever the driver's own state has got to: from part way
        // down at most `down`, from down `up`, and a step for the rounding.
        drive_unseen(
            animator,
            driver,
            rig,
            frame,
            gesturing,
            emote::GET_UP,
            down + up + UNSEEN_STEP,
        );
        drive_unseen(
            animator,
            driver,
            rig,
            frame,
            gesturing,
            was,
            way_into(down, at),
        );
        std::mem::swap(&mut gesturing.own, &mut own);
    }
    0.0
}

/// Seconds into a state's way down of `down` seconds that put a beast
/// `through` of the way down: past the end for all of it, so the driver is
/// holding the state rather than a rounding short of the end, from which it
/// would come back up the way it went instead of rising as the animal does.
fn way_into(down: f32, through: f32) -> f32 {
    if through >= 1.0 {
        down + UNSEEN_STEP
    } else {
        through.max(0.0) * down
    }
}

/// Drives a body through `seconds` of frames nobody is shown, asking `ask`
/// of the first, and stops early once its driver is playing nothing; answers
/// how many seconds it ran. `gesturing` is the frame's, and is left as it was.
fn drive_unseen<'a>(
    animator: &Animator,
    driver: &mut AvatarDriver,
    rig: &Rig,
    frame: &Frame<'_>,
    gesturing: &mut Gesturing<'a>,
    ask: &'a str,
    seconds: f32,
) -> f32 {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a few seconds of sixtieths, and none for a negative span"
    )]
    let steps = (seconds / UNSEEN_STEP).ceil().max(0.0) as usize;
    if steps == 0 {
        return 0.0;
    }
    #[expect(clippy::cast_precision_loss, reason = "a few hundred steps")]
    let step = seconds / steps as f32;
    let unseen = Frame {
        delta: step,
        ..*frame
    };
    let asking = gesturing.ask.replace(ask);
    let mut ran = 0.0;
    for _ in 0..steps {
        drive_body(animator, driver, rig, &unseen, gesturing);
        gesturing.ask = None;
        ran += step;
        if driver.gesture().is_none() {
            break;
        }
    }
    gesturing.ask = asking;
    ran
}

/// What one body's frame is, beside the window that is steering it.
///
/// A struct rather than eight arguments, because half of them are decided once
/// for the whole frame and passing them one at a time is how a caller comes to
/// hand two bodies different cycles.
struct Frame<'a> {
    /// Seconds since the last frame.
    delta: f32,
    /// Where in the cycle every body is held this frame.
    cycle: f32,
    /// Whether the gait is what is carrying the body.
    gaiting: bool,
    /// The gait the picker chose, for this body's rig.
    picked: &'a Gait,
    /// The baked clip riding over the motion, if one is playing.
    clip: Option<&'a symbios_avatar::PoseClip>,
    /// The jaw's pivot angle this frame, in radians.
    jaw_angle: f32,
    /// The face the body is resting in.
    showing: Expression,
    /// This body's eyes, if it has any.
    eyes: Option<&'a symbios_avatar::Eyes>,
}

/// Drives one body for one frame, with the two layers only this crate supplies.
///
/// Split from [`steer`] because the closures below are what make this long: a
/// gesture, a clip, a gaze and a face all ride over the driver's motion, and
/// each of them has to say where in the order it goes.
fn drive_body(
    animator: &Animator,
    driver: &mut AvatarDriver,
    rig: &Rig,
    frame: &Frame<'_>,
    gesturing: &Gesturing<'_>,
) -> Option<symbios_avatar::anim::driver::Driven> {
    let (delta, cycle, clip) = (frame.delta, frame.cycle, frame.clip);
    let speed = animator.speed_of(rig, frame.picked);
    // **Both gaze layers stand aside for a gesture that aims the head,
    // and only for one that does** (#30). Everything in the settled
    // layer writes the head outright — `look_at` assigns a chest, neck
    // and head rotation rather than composing one — so a nod applied
    // under it arrived correct and was put back level a moment later,
    // which is a gesture this window could not show at all.
    //
    // Asked of the clip rather than of the gesture's name, because the
    // engine already answers it: a clip that aims the head carries a
    // `Target::Gaze` track and one that does not, does not. So a wave
    // still lets the body look around while it waves — which is what a
    // waving body does — and a nod owns the head for as long as it runs.
    //
    // A gesture the driver plays aims the head itself and stands its own
    // glance aside for it, so all this window has to do then is not turn
    // the head away (#53).
    let aimed = gesturing.own_aims();
    let own = gesturing.own.as_ref();
    // Over the locomotion and before the contacts are settled, which is
    // where authored motion goes: the legs keep the walk that carries
    // them, and the feet are planted after whatever was laid on top
    // (engine #253's order).
    let over_locomotion = |rig: &Rig, pose: &mut Pose| {
        lay_gesture(rig, own, pose);
        if let Some(clip) = clip {
            // After the gesture, so the two clip forms layer in the
            // order the engine describes them: goals first, angles over
            // them. `PoseClip::apply` writes only the joints its own
            // tracks name, which is what lets an imported gesture ride a
            // procedural walk.
            clip.apply(rig, pose, cycle * clip.duration());
            if animator.in_place {
                pose.translation.x = 0.0;
                pose.translation.z = 0.0;
            }
        }
    };
    // Over the settled body and before the blend: a face and a
    // deliberate gaze cannot fight the footing solve, and a transition
    // should correct what they produced rather than be overwritten.
    let over_settled = |rig: &Rig, pose: &mut Pose| {
        if aimed {
            // The driver's own glance ran a moment ago and writes the
            // head outright; it cannot know about a gesture this window
            // laid on itself, so the aim is put back. Re-applying the
            // whole gesture rather than its gaze track alone, because
            // the clip is the only thing that knows which tracks those
            // are and every other track re-applies to the same value.
            lay_gesture(rig, own, pose);
        } else if !gesturing.driver_aims {
            // A target at head height, applied after the gait, because
            // looking somewhere is a turn added to whatever the spine is
            // already doing.
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
                pose,
                target,
                &GazeConfig {
                    limit: animator.gaze_limit,
                    ..GazeConfig::default()
                },
            );
        }
        // The face, after the gaze for the same reason the gaze comes
        // after the gait: everything here is added to wherever the head
        // already is.
        pose_face(rig, pose, frame.jaw_angle, frame.showing, animator.viseme);
    };

    // **`Carriage::Own`, which is the viewer's whole difference from an
    // application** (engine's own `Carriage`): nothing else moves this
    // body, so a leap flies and a walk derives every stance offset
    // afresh rather than holding world points — a foothold ledger on a
    // body walking on the spot would pin a treadmill's feet to the floor.
    // The synthetic chassis in `examples/viewer.rs` is what asks for the
    // other answer, and it is the one flag that changes this.
    driver.set_config(DriverConfig {
        carriage: animator.carriage,
        idle: animator.idle,
        blend: animator.blend,
        pace_response: animator.pace_response,
        // One number for how long a gesture takes, whoever plays it: a
        // beast plays what it was asked for while it was down once it is up.
        gesture_secs: GESTURE_TIME,
        // And no rate limit. It is for a sender pasting one word four times;
        // a pick in this window is one hand's deliberate choice, and dropping
        // one would leave the row lit for a gesture nothing plays.
        gesture_cooldown: 0.0,
        ..DriverConfig::default()
    });
    // A listener goes stiller than a body alone in a room, and a body
    // that is speaking is a body whose idle is the speaking one — which
    // is why the talking variant follows `talking` rather than having a
    // switch of its own.
    driver.set_idle_config(if animator.talking {
        IdleConfig::talking()
    } else if animator.listening {
        IdleConfig::listening()
    } else {
        IdleConfig::default()
    });
    let inputs = Inputs {
        delta,
        // The magnitude is what picks the gait and its speed; the
        // direction the body TRAVELS is `heading`, so this points down
        // the body's own forward and says only how fast.
        velocity: Vec3::new(
            0.0,
            animator.vertical,
            if frame.gaiting { speed } else { 0.0 },
        ),
        at: animator.at,
        facing: animator.heading(),
        heading: Some(Heading::degrees(animator.heading)),
        // A swim replaces the walk rather than layering over it, for the
        // same reason a leap does: a body cannot be mid-stride and prone
        // in the water at once. Both are shown rather than inferred,
        // because a viewer has no world to infer them from.
        showing: animator
            .swim
            .map(Showing::Swim)
            .or_else(|| animator.leap.map(Showing::Leap)),
        cycle: Some(cycle),
        // A beast's state, and what gets it up out of one: see
        // `plan_gesture`. Everything else this window lays on itself.
        gesture: gesturing.ask,
        gait: (!matches!(animator.gait, GaitKind::Natural | GaitKind::Pattern(_)))
            .then_some(frame.picked),
        pattern: match animator.gait {
            GaitKind::Pattern(pattern) => Some(pattern),
            _ => None,
        },
        walk: WalkFlags {
            posture: animator.posture,
            head_level: animator.head_level,
            footing: animator.footing.then(FootingConfig::default),
            // Only while turning, and only while the postural layer is
            // on. A gaze led down a straight path is a target the head
            // already points at, so switching it on there would cost
            // nothing and say nothing; switching it on with the posture
            // off would put a head turn on a body deliberately being
            // shown as bare legs.
            gaze: (animator.posture && animator.turn != 0.0).then(GazeConfig::default),
        },
        turn: animator.turn.to_radians(),
        // A blink is stochastic, so a single captured frame almost never
        // catches one; holding the lids at a chosen point is what makes
        // the geometry path checkable from a still. Either way the phase
        // runs THROUGH the resting face, because adding a widened rest
        // to a full blink leaves an eye that never shuts.
        lids: (!animator.blinking).then_some(animator.closure),
        face: frame.showing,
        eyes: frame.eyes,
        over_locomotion: Some(&over_locomotion),
        over_settled: Some(&over_settled),
        ..Inputs::default()
    };
    driver.drive(rig, &inputs, sloping(animator.grade, animator.camber))
}

/// Writes the face's pose layers in their contract order.
///
/// Speech's jaw first; then the resting expression, which COMPOSES its jaw
/// bias over speech (a happy body keeps its parted rest while talking) and
/// owns the brows and corners outright; then a held viseme over the mouth,
/// because speech owns it — a viseme writes the jaw and
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

/// The jaw's pivot: the parent of the marker chain's tip.
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

/// Where a tracked gaze points, `elapsed` seconds into its scan.
///
/// A continuous scan, not a lap. A target circling one way forever sweeps the
/// head to its limit, snaps across as the target passes behind it, and sweeps
/// again. A triangle wave runs the same arc at the same `speed` in both
/// directions and reverses at the ends, which is the scanning loop this control
/// is for. Phase-offset by one span so it starts at zero moving positive;
/// speed 0 holds it there.
fn scanned_angle(elapsed: f32, speed: f32, limit: f32) -> f32 {
    let span = limit.clamp(0.01, std::f32::consts::PI);
    let along = (span + elapsed * speed).rem_euclid(4.0 * span);
    if along < 2.0 * span {
        along - span
    } else {
        3.0 * span - along
    }
}

/// Moves the gesture's own clock by `delta`.
///
/// **A gesture is the one motion here with a clock of its own.** A greeting
/// happens once and finishes; running it on [`Animator::cycle`] would loop it
/// forever and play it at whatever speed the legs happen to be going. It holds
/// at its end rather than clearing itself, so a body that has waved is left
/// with its arm back at rest and the picker still says which gesture it made —
/// which is also why it is not routed through [`Inputs::gesture`], where a
/// request fires once and clears and a capture could not hold it at a phase.
///
/// **Except a pick a driver is playing, or a beast's state** (#53), which
/// keeps its own time, whether or not the driver has it yet: `resting` says
/// the pick is one. The number is then the driver's progress, read back
/// after its frame rather than run here.
///
/// The write is guarded on the gesture actually running, which is what keeps
/// the change-detection signal in [`steer`] from latching on.
fn advance_gesture(animator: &mut Animator, delta: f32, resting: bool) {
    let held = animator.scrub || animator.told.handed || resting;
    if let Some((_, through)) = &mut animator.gesture
        && !held
    {
        *through = (*through + delta / GESTURE_TIME).min(1.0);
    }
}

/// Lays the procedural gesture this window plays over whatever the body is
/// already doing.
///
/// **Over, and before the baked clip**, so the two clip forms layer in the
/// order the engine describes them: goals first, angles over them. A gesture
/// writes only the parts it addresses, which is what lets a body wave while it
/// walks — and what lets it wave at all on a body that has a hand free, and not
/// on one that has none.
///
/// Whether it aimed the head is [`steer`]'s to ask, of the clip rather than of
/// the name — a clip that aims the head carries a [`Target::Gaze`] track, and
/// asking it beats keeping a list of which gestures involve the head, which
/// would be wrong the moment the roster grew.
fn lay_gesture(rig: &Rig, own: Option<&(Clip, f32)>, pose: &mut Pose) {
    if let Some((gesture, through)) = own {
        gesture.apply(rig, pose, *through);
    }
}

/// Which way the sloped ground faces, for a given grade and camber.
///
/// **The one place the plane is defined, and that is the point of it.** The
/// ground the feet are solved against and the floor the viewer draws are two
/// expressions of a single surface, and they have drifted apart twice — once
/// with the drawn tilt rotating the opposite way to the solved one, once square
/// to it, after the solved surface moved axis and the drawn one stayed where it
/// was. Both times the two were kept in agreement by a comment saying they had
/// to be. A comment is not a mechanism.
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
/// **Grade runs along Z, the way the body walks.** X is the body's lateral axis
/// — the engine's forward is `+Z` and [`Stride::for_body`] strides down it — so
/// tilting X would ask the grade slider's question about a camber the body
/// stands across rather than a hill it climbs. Camber is that second axis on
/// purpose, so the pair reaches every plane.
///
/// Shared by the footing solve and by the stride, which seats its stride on
/// whatever ground it is given: handing those two
/// different floors is exactly what leaves a swing arc at the rest ground height
/// while the plant settles onto a hill.
fn sloping(grade: f32, camber: f32) -> impl Fn(Vec3) -> Option<Ground> + Copy {
    let normal = ground_normal(grade, camber);
    move |foot: Vec3| {
        Some(Ground {
            position: Vec3::new(foot.x, foot.x * camber + foot.z * grade, foot.z),
            normal,
        })
    }
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
    // Which of the row's gestures are states on this body (#53), which the
    // row says, and whose slider works otherwise.
    let states: Vec<&str> = bodies.iter().next().map_or_else(Vec::new, |body| {
        emote::ROSTER
            .iter()
            .copied()
            .filter(|name| state(&body.avatar.rig, name))
            .collect()
    });
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
            locomotion_section(ui, &clips, &mut animator, legs, &states);
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
    states: &[&str],
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
        pattern_picker(ui, animator);
    }
    if !standing {
        // **A checkbox rather than a bare slider**, because the default answer
        // is now "whatever this speed implies" and a slider alone could not say
        // that. Ticking it takes the cadence off the speed axis on purpose,
        // which is a thing to do to a walk — slow the cursor and watch a foot
        // plant — rather than a claim about one.
        ui.horizontal(|ui| {
            let mut naming = animator.cadence.is_some();
            if ui.toggle_value(&mut naming, "cadence /s").changed() {
                animator.cadence = naming.then_some(1.1);
            }
            if let Some(cadence) = &mut animator.cadence {
                ui.add(egui::Slider::new(cadence, 0.05..=3.0).text(""));
            } else {
                ui.label("from the speed");
            }
        });
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
        walking_controls(ui, animator);
    }
    // **The one axis a swim has**, and the whole of what there is to look at:
    // zero treads water and the top of the range is a body swimming flat out.
    // The engine reads it in metres per second and normalises by the body's own
    // length, so the same slider means the same stroke on a child and a giant.
    if let Some(swim) = &mut animator.swim {
        ui.add(egui::Slider::new(&mut swim.pace, 0.0..=2.0).text("m/s"));
        ui.toggle_value(&mut swim.carriage, "carriage");
    }

    gesture_row(ui, animator, legs, states, !standing);
}

/// A four-legged body's gait: by speed, as the body would choose for itself,
/// or one pattern held whatever the speed (#370).
#[cfg(feature = "editor")]
fn pattern_picker(ui: &mut bevy_egui::egui::Ui, animator: &mut Animator) {
    ui.horizontal_wrapped(|ui| {
        if ui
            .selectable_label(animator.gait == GaitKind::Natural, "by speed")
            .clicked()
        {
            animator.gait = GaitKind::Natural;
        }
        for kind in GaitKind::PATTERNS.into_iter().chain([GaitKind::Wave]) {
            let picked = animator.gait == kind;
            if ui.selectable_label(picked, kind.label()).clicked() {
                animator.gait = kind;
            }
        }
    });
}

/// The gestures a body can be asked for, and how far into one it is.
///
/// On a beast, `states` names the gestures that are states (#53): its driver
/// plays them, so the slider follows it rather than leading, and poses one
/// only under hold - which is offered beside it, because the phase row's hold
/// is not drawn for a body standing still, and a beast that is not standing
/// still does not sit.
///
/// **And hold is offered here whenever it is on and `phase_hold` says the
/// phase row is not offering it** (#53): a hold left on with nothing drawn to
/// turn it off - a state's let go of under it, or `--phase` on a standing
/// body - froze every later gesture at nought, a person's included.
#[cfg(feature = "editor")]
fn gesture_row(
    ui: &mut bevy_egui::egui::Ui,
    animator: &mut Animator,
    legs: usize,
    states: &[&str],
    phase_hold: bool,
) {
    use bevy_egui::egui;
    // **The gestures sit apart from the locomotion**, because they are not one
    // of the things being chosen between: a gesture is laid over whatever the
    // body is already doing, so it is its own row rather than another entry in
    // the picker above.
    ui.separator();
    ui.label(egui::RichText::new("gesture").strong());
    // A beast's roster on four legs (#370), a person's otherwise.
    let roster = if legs == 4 {
        emote::ROSTER
    } else {
        gesture::ROSTER
    };
    ui.horizontal_wrapped(|ui| {
        for name in roster {
            let playing = animator
                .gesture
                .as_ref()
                .is_some_and(|(chosen, _)| chosen == name);
            let label = ui.selectable_label(playing, *name);
            let label = if states.contains(name) {
                label.on_hover_text(
                    "a state: the beast goes down at its own pace and stays \
                     down. Get Up, letting go of it or another gesture gets it \
                     up, and the other gesture plays once it is; moving it gets \
                     it up in a hurry, and it will not go down while it moves",
                )
            } else if *name == emote::GET_UP {
                label.on_hover_text("gets a sitting or lying beast up where it is")
            } else if playing && animator.told.busy {
                label.on_hover_text(
                    "the beast is playing it, and finishes what it has started; \
                     another gesture plays in its place",
                )
            } else {
                label
            };
            if label.clicked() {
                click_gesture(animator, name);
            }
        }
    });
    let handed = animator.told.handed;
    let state = animator
        .gesture
        .as_ref()
        .is_some_and(|(name, _)| states.contains(&name.as_str()));
    let hold = state || (animator.scrub && !phase_hold);
    if animator.gesture.is_none() && !hold {
        return;
    }
    ui.horizontal(|ui| {
        if let Some((_, through)) = &mut animator.gesture {
            // Followed while the driver plays it, and led only where this
            // window poses it: a state under hold.
            ui.add_enabled(
                !handed || (state && animator.scrub),
                egui::Slider::new(through, 0.0..=1.0)
                    .text("through")
                    .fixed_decimals(2),
            )
            .on_disabled_hover_text(
                "the beast keeps its own pace: a state's way down, or a \
                 gesture played once it is up. Hold poses a state at any \
                 point of its way down",
            );
        }
        if hold {
            ui.toggle_value(&mut animator.scrub, "hold");
        }
    });
}

/// What a click on the gesture row's `name` does: picks it, or lets go of
/// it if it is the pick.
///
/// **Except a gesture a beast's driver is playing that is not a state**
/// (#53), which the engine plays to its end once started and has no way to
/// stop: let go of, the row went dark over a beast still shaking. So it stays
/// lit until it ends; another pick still plays in its place. One asked for
/// while the beast was down and not yet playing is let go of as any pick
/// is, and the driver drops it: letting go asks for `emote::GET_UP`, which
/// clears what it was to play once up.
#[cfg(feature = "editor")]
fn click_gesture(animator: &mut Animator, name: &str) {
    let lit = animator
        .gesture
        .as_ref()
        .is_some_and(|(chosen, _)| chosen == name);
    if lit && animator.told.busy {
        return;
    }
    animator.gesture = (!lit).then(|| (name.to_string(), 0.0));
}

/// The controls that only mean anything while the gait is walking: its pace,
/// and the two postural ablations.
#[cfg(feature = "editor")]
fn walking_controls(ui: &mut bevy_egui::egui::Ui, animator: &mut Animator) {
    use bevy_egui::egui;
    ui.add(egui::Slider::new(&mut animator.pace, 0.0..=2.0).text("pace"));
    ui.horizontal(|ui| {
        ui.toggle_value(&mut animator.posture, "posture");
        // The ablation only means anything while the posture layer runs —
        // with it off there is no lean for the neck to bargain over.
        ui.add_enabled_ui(animator.posture, |ui| {
            ui.toggle_value(&mut animator.head_level, "head level")
                .on_hover_text(
                    "the neck takes the trunk's lean back off so the body \
                     looks where it is going; off, the head goes down with \
                     the trunk — an ablation for judging the hunch, not a \
                     look",
                );
        });
    });
}

/// The ground the body meets: the footing solve, its slope, and the readout.
#[cfg(feature = "editor")]
fn ground_section(ui: &mut bevy_egui::egui::Ui, animator: &mut Animator) {
    use bevy_egui::egui;
    ui.label(egui::RichText::new("ground").strong());
    ui.horizontal(|ui| {
        ui.toggle_value(&mut animator.footing, "footing");
        // The readout the locomotion question should be settled on. A body
        // that strains occasionally is a body on hard ground; one that strains
        // constantly is a body whose goals are wrong. It used to say how far
        // the solve moved the feet as well — see [`Animator::strained`] for
        // where those figures went.
        ui.label(if animator.strained {
            "straining"
        } else {
            "reaching"
        });
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
    ///
    /// **Without `TimePlugin`, deliberately.** A driven frame is a function of
    /// how long it was, and left to the real clock a test frame is a few
    /// microseconds — so a transition asked to take a fifth of a second gets
    /// through a ten-thousandth of itself per frame and reads, from outside, as
    /// a blend that never started. That cost a wrong diagnosis once. Disabling
    /// the plugin leaves `Time` to [`tick`], which is what makes these frames a
    /// fixed sixtieth of a second and the results the same on a fast machine
    /// and a slow one.
    fn app() -> App {
        app_of(Archetype::default())
    }

    /// The same around one beast, the default dog (#53).
    fn beast_app() -> App {
        app_of(Archetype::Quadruped(
            symbios_avatar::QuadrupedParams::default(),
        ))
    }

    /// The same around one body of `archetype`.
    fn app_of(archetype: Archetype) -> App {
        let mut app = bare_app();
        app.world_mut()
            .spawn(SpawnAvatar::from(AvatarRecord::new("Driven", archetype)));
        tick(&mut app);
        app
    }

    /// The same with no body in it yet.
    fn bare_app() -> App {
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins.build().disable::<bevy::time::TimePlugin>(),
            AssetPlugin::default(),
            bevy::mesh::MeshPlugin,
            bevy::image::ImagePlugin::default(),
        ))
        .init_resource::<Time>()
        .init_asset::<StandardMaterial>()
        .init_asset::<SkinnedMeshInverseBindposes>()
        .init_resource::<Animator>()
        // Empty by default; the tests that need clips insert their own, which is
        // also the shape a consumer fetching them at run time uses.
        .init_resource::<Clips>()
        .init_resource::<Wrote>()
        .add_systems(
            Update,
            (build_requested_avatars, adopt_bodies, steer, count_writes).chain(),
        );
        app
    }

    /// One frame, a sixtieth of a second long.
    fn tick(app: &mut App) {
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_secs_f32(1.0 / 60.0));
        app.update();
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
        tick(&mut app);
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
            tick(&mut app);
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
        tick(&mut app);
        tick(&mut app);
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
        tick(&mut app);
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
        tick(&mut app);
        tick(&mut app);
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
        tick(&mut app);
        tick(&mut app);
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
            tick(app);
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

    #[test]
    fn switching_source_blends_and_a_zero_blend_snaps() {
        // What #141 lists as a thing to watch — what a transition costs —
        // asserted at both ends. A blend that never starts is a snap, and a
        // snap that blends anyway is a slider that does nothing.
        //
        // **Read off the drawn pose rather than off a transition object**, and
        // that is the shape of the change #43 made rather than a weaker test.
        // The blend belongs to `AvatarDriver` now and it keeps no per-body
        // component to look inside, so what is asserted is what a viewer can
        // actually see: one frame after a source switch, a blended body is
        // still near the pose it is leaving and a snapping one has arrived at
        // the pose it is going to.
        //
        // The switch is walk-to-stand, which is a change of source the driver
        // owns. A CLIP switch no longer blends: a clip is a layer over the
        // motion in the new division, and only the thing that owns the motion
        // can start a transition through it. That is a real loss and it is
        // written down on #43 rather than hidden here.
        let leaving = |blend: f32| {
            let mut app = app();
            {
                let mut animator = app.world_mut().resource_mut::<Animator>();
                animator.walking = true;
                animator.blinking = false;
                animator.tracking = false;
                animator.blend = blend;
            }
            // **Walked in first, and the count is not padding.** The body
            // leaves rest through a source change of its own, and a transition
            // still running when the switch under test arrives would put the
            // first blend into the second's reading. Thirty frames is half a
            // second against a fifth of one.
            for _ in 0..30 {
                tick(&mut app);
            }
            let walking = posed(&mut app);
            app.world_mut().resource_mut::<Animator>().walking = false;
            tick(&mut app);
            (walking.clone(), posed(&mut app))
        };

        let (walking, blended) = leaving(0.2);
        let (snapped_from, snapped) = leaving(0.0);
        // The two runs walk to the same place, so the poses they leave are
        // comparable — if they were not, the comparison below would be reading
        // a difference in where the bodies started.
        assert!(
            apart_by(&walking, &snapped_from) < 1e-3,
            "the two runs did not leave the same pose, so nothing below compares"
        );
        assert!(
            apart_by(&blended, &walking) < apart_by(&snapped, &walking),
            "a blended switch left the walk no more gently than a snap: \
             blended moved {:.4} rad, snapped {:.4}",
            apart_by(&blended, &walking),
            apart_by(&snapped, &walking)
        );
        assert!(
            apart_by(&snapped, &blended) > 1e-4,
            "the blend slider changed nothing at all"
        );
    }

    /// The pose the one body is drawn in.
    fn posed(app: &mut App) -> Pose {
        let mut bodies = app.world_mut().query::<&AvatarPose>();
        bodies
            .iter(app.world())
            .next()
            .expect("a posed body")
            .0
            .clone()
    }

    /// The furthest any joint of `a` is turned from the same joint of `b`.
    fn apart_by(a: &Pose, b: &Pose) -> f32 {
        a.rotations
            .iter()
            .zip(&b.rotations)
            .map(|(a, b)| a.angle_between(*b))
            .fold(0.0f32, f32::max)
    }

    /// The one body's rig.
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
        tick(&mut app);
        assert_eq!(
            app.world().resource::<Wrote>().0,
            1,
            "the frame a still pose was asked for did not write it"
        );

        tick(&mut app);
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
        tick(&mut app);
        tick(&mut app);
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
        tick(&mut app);
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
        tick(&mut app);
        tick(&mut app);
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
        tick(&mut app);
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
            // **Long enough for the transition to land.** A swim is its own
            // motion family, so entering one is a source change the driver
            // blends — it did not before #43, when this window switched
            // motions with no transition between them and a single frame was
            // enough to read the new one. Twenty frames is a third of a second
            // against a blend of 0.15, and the cycle is scrubbed, so nothing
            // else moves while they pass.
            for _ in 0..20 {
                tick(app);
            }
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
            tick(app);
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
                tick(app);
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
    #[test]
    fn every_pattern_answers_to_its_own_name() {
        // #370. A beast's patterns are reachable by name from `--gait` as the
        // picker offers them, the spaces in a name written as dashes - all
        // but "trot", which reaches the five's own diagonal gait first, the
        // same footfalls at a fixed duty.
        for kind in GaitKind::PATTERNS {
            for name in [kind.label().to_string(), kind.label().replace(' ', "-")] {
                let found = GaitKind::named(&name);
                assert!(
                    found == Some(kind) || (name == "trot" && found == Some(GaitKind::Trot)),
                    "{name} answered {found:?}"
                );
            }
        }
        assert_eq!(
            GaitKind::named("canter"),
            Some(GaitKind::Pattern(Pattern::Canter(Lead::Right)))
        );
    }

    #[test]
    fn a_beasts_emote_left_held_neither_moves_a_person_nor_stays() {
        // #370. The picker holds a gesture at its end, so a dog's sit
        // outlived the dog, and the person's row, a person's roster, had no
        // way to clear it. A gesture is looked up by body, so the person
        // stands as one asked for nothing, and one no body in the window can
        // make is let go.
        let ask = |gesture: Option<&str>| {
            let mut app = app();
            app.world_mut().resource_mut::<Animator>().gesture =
                gesture.map(|name| (name.to_string(), 1.0));
            tick(&mut app);
            let held = app.world().resource::<Animator>().gesture.clone();
            (posed(&mut app), held)
        };
        let (sitting, held) = ask(Some("Sit"));
        let (standing, _) = ask(None);
        assert_eq!(held, None, "a person kept a beast's sit");
        assert!(sitting == standing, "a beast's sit moved a person");
    }

    #[test]
    fn a_pattern_left_picked_walks_a_person_as_a_person() {
        // #370. The pattern row is drawn on four legs only, so a pattern
        // picked on a dog stayed picked when a person was loaded, and the
        // person's speed was worked out through the pattern's duty.
        let mut app = app();
        let mut bodies = app.world_mut().query::<&AvatarBody>();
        let rig = bodies
            .iter(app.world())
            .next()
            .expect("a body")
            .avatar
            .rig
            .clone();
        for kind in GaitKind::PATTERNS {
            assert_eq!(kind.of(&rig), GaitKind::Natural.of(&rig), "{kind:?}");
        }
    }

    /// Picks a gesture in the row, or lets go of the pick, as a click does.
    fn pick(app: &mut App, name: Option<&str>) {
        app.world_mut().resource_mut::<Animator>().gesture =
            name.map(|name| (name.to_string(), 0.0));
    }

    /// The pick the row holds.
    fn picked(app: &App) -> Option<(String, f32)> {
        app.world().resource::<Animator>().gesture.clone()
    }

    /// What the one body's driver is playing, and how long it has.
    fn playing(app: &mut App) -> Option<(String, f32)> {
        let mut drivers = app.world_mut().query::<&AvatarDriver>();
        drivers
            .iter(app.world())
            .next()
            .and_then(|driver| driver.gesture())
            .map(|(name, elapsed)| (name.to_string(), elapsed))
    }

    /// Where every joint of the one body is drawn.
    fn drawn(app: &mut App, rig: &Rig) -> Vec<Vec3> {
        posed(app).forward(rig).positions
    }

    /// How high the body's hips are drawn.
    fn hips(app: &mut App, rig: &Rig) -> f32 {
        let pelvis = *rig.in_zone(Zone::Pelvis).first().expect("a pelvis");
        drawn(app, rig)[pelvis].y
    }

    /// Runs `frames` frames and answers the furthest any joint moved in one
    /// of them: a snap is a body's worth of travel in a frame.
    fn worst_step(app: &mut App, rig: &Rig, frames: usize) -> f32 {
        let mut last = drawn(app, rig);
        let mut worst = 0.0f32;
        for _ in 0..frames {
            tick(app);
            let now = drawn(app, rig);
            let step = last
                .iter()
                .zip(&now)
                .map(|(a, b)| a.distance(*b))
                .fold(0.0f32, f32::max);
            worst = worst.max(step);
            last = now;
        }
        worst
    }

    /// Frames in `seconds`, rounded up.
    fn frames(seconds: f32) -> usize {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a few seconds of frames"
        )]
        let frames = (seconds * 60.0).ceil() as usize;
        frames
    }

    /// A beast standing still, blinking and scanning nothing, sat down and
    /// held there for `held` seconds; and how high its hips stood first.
    fn sat(held: f32) -> (App, Rig, f32) {
        let mut app = beast_app();
        {
            let mut animator = app.world_mut().resource_mut::<Animator>();
            animator.blinking = false;
            animator.tracking = false;
        }
        tick(&mut app);
        let rig = rig_of(&mut app);
        let standing = hips(&mut app, &rig);
        pick(&mut app, Some("Sit"));
        let down = emote::seconds(&rig, "Sit").expect("a beast sits");
        for _ in 0..frames(down + held) {
            tick(&mut app);
        }
        (app, rig, standing)
    }

    #[test]
    fn a_beasts_sit_goes_down_at_its_own_pace_and_is_held() {
        // #53, engine #377. The window ran every gesture over its own second
        // and a half, so a dog's sit landed half a second late, and a state
        // is the driver's to play. Run with nothing else moving, so the
        // still-body rule has to know a driver's state is motion: it writes
        // nothing to the window to wake it.
        let mut app = beast_app();
        {
            let mut animator = app.world_mut().resource_mut::<Animator>();
            animator.blinking = false;
            animator.tracking = false;
        }
        tick(&mut app);
        let rig = rig_of(&mut app);
        let standing = hips(&mut app, &rig);
        let down = emote::seconds(&rig, "Sit").expect("a beast sits");
        pick(&mut app, Some("Sit"));
        for _ in 0..frames(down / 2.0) {
            tick(&mut app);
        }
        let half = hips(&mut app, &rig);
        // Two frames past its own seconds, for the frame the pick is asked on.
        for _ in frames(down / 2.0)..frames(down) + 2 {
            tick(&mut app);
        }
        let landed = hips(&mut app, &rig);
        let through = picked(&app).expect("still picked").1;
        for _ in 0..frames(3.0) {
            tick(&mut app);
        }
        let held = hips(&mut app, &rig);
        let depth = standing - held;
        assert!(depth > 0.1, "the sit lowered the hips {depth} m");
        assert!(
            (standing - half) > 0.1 * depth && (standing - half) < 0.9 * depth,
            "half way through its time the sit was {:.0}% down",
            (standing - half) / depth * 100.0
        );
        assert!(
            (landed - held).abs() < 2.0e-3,
            "at its own {down:.2} s the sit stood {:.1} mm off where it holds",
            (landed - held) * 1000.0
        );
        assert!(
            (through - 1.0).abs() < 1.0e-6,
            "the slider read {through} at the end of the way down"
        );
        assert_eq!(
            playing(&mut app).map(|(name, _)| name).as_deref(),
            Some("Sit"),
            "three seconds on, the beast is no longer in its sit"
        );
    }

    #[test]
    fn letting_go_of_a_sit_or_asking_get_up_rises_without_a_snap() {
        // #53. Letting go of the pick set no gesture at all, so a sitting dog
        // stood up in one frame. Both ways out get the beast up in the
        // driver's own order and time, and "Get Up" lets go of itself.
        for route in [None, Some(emote::GET_UP)] {
            let (mut app, rig, standing) = sat(1.0);
            let up = emote::getting_up(&rig, "Sit")
                .expect("a sit to rise from")
                .1;
            pick(&mut app, route);
            let rising = worst_step(&mut app, &rig, frames(up / 2.0));
            let midway = playing(&mut app);
            let rest = worst_step(&mut app, &rig, frames(up / 2.0 + 0.5));
            let worst = rising.max(rest);
            assert!(
                midway.is_some_and(|(name, _)| name == "Sit"),
                "{route:?}: half way through its rise the beast was not rising"
            );
            assert!(
                worst < 0.1,
                "{route:?}: a joint moved {:.0} mm in one frame getting up",
                worst * 1000.0
            );
            assert_eq!(playing(&mut app), None, "{route:?}: it never got up");
            assert_eq!(picked(&app), None, "{route:?}: the row still holds a pick");
            let stood = hips(&mut app, &rig);
            assert!(
                (stood - standing).abs() < 0.01,
                "{route:?}: up, the hips stood {:.0} mm off where they began",
                (stood - standing) * 1000.0
            );
        }
    }

    #[test]
    fn a_gesture_picked_while_down_plays_once_the_beast_is_up() {
        // #53. Another pick replaced the sit outright, so the dog stood up in
        // one frame to shake itself. The driver gets it up first - exactly as
        // it gets up from a sit let go of, with nothing of the shake laid
        // over it and the shake's clock not yet running - and then plays it;
        // a pick while it plays is played in its place, as a click on a
        // person's row is; and once one is done the row holds it at its end,
        // as it holds a gesture this window plays. A shake because it braces
        // from its first moment, so one laid over the rise shows.
        let (mut app, rig, _) = sat(1.0);
        let (mut plain, _, _) = sat(1.0);
        let up = emote::getting_up(&rig, "Sit")
            .expect("a sit to rise from")
            .1;
        pick(&mut app, Some("Shake"));
        pick(&mut plain, None);
        for _ in 0..frames(up) - 2 {
            tick(&mut app);
            tick(&mut plain);
            assert!(
                posed(&mut app) == posed(&mut plain),
                "the window laid the shake over the beast getting up"
            );
        }
        let rising = playing(&mut app);
        let early = picked(&app).expect("still picked").1;
        let shaking = worst_step(&mut app, &rig, 6);
        let shake = playing(&mut app);
        pick(&mut app, Some("Wag"));
        tick(&mut app);
        let wag = playing(&mut app);
        let rest = worst_step(&mut app, &rig, frames(GESTURE_TIME));
        assert!(
            rising.is_some_and(|(name, _)| name == "Sit"),
            "the shake did not wait for the beast to get up"
        );
        assert!(
            early < 0.05,
            "the shake's clock ran to {early} before it played"
        );
        assert!(
            shake.is_some_and(|(name, _)| name == "Shake"),
            "the beast got up and did not shake"
        );
        assert!(
            wag.is_some_and(|(name, _)| name == "Wag"),
            "a pick while the beast shook was dropped"
        );
        let worst = shaking.max(rest);
        assert!(
            worst < 0.1,
            "a joint moved {:.0} mm in one frame",
            worst * 1000.0
        );
        assert_eq!(playing(&mut app), None, "the wag never finished");
        assert_eq!(
            picked(&app),
            Some(("Wag".to_string(), 1.0)),
            "a played gesture is held at its end"
        );
    }

    #[test]
    fn walking_a_sitting_beast_gets_it_up_in_a_hurry_and_lets_the_pick_go() {
        // #53. A sit left picked under a walk sat the dog down over its own
        // legs. Moved, it gets up in two fifths of its own time, the row lets
        // go of a state it is no longer in, and a sit asked of it while it
        // walks is let go rather than kept lit over a walking body.
        let (mut app, rig, _) = sat(1.0);
        let up = emote::getting_up(&rig, "Sit")
            .expect("a sit to rise from")
            .1;
        app.world_mut().resource_mut::<Animator>().walking = true;
        let rising = worst_step(&mut app, &rig, frames(up * emote::HURRY) + 2);
        assert_eq!(
            playing(&mut app),
            None,
            "a walked beast was still getting up"
        );
        assert_eq!(
            picked(&app),
            None,
            "the row kept a sit the beast got up from"
        );
        // Up in two fifths of its own time: its fastest joint moves 106 mm
        // a frame, the engine's own hurry, and a snap moves it the whole way.
        assert!(
            rising < 0.11,
            "a joint moved {:.0} mm in one frame getting up",
            rising * 1000.0
        );
        pick(&mut app, Some("Sit"));
        tick(&mut app);
        assert_eq!(playing(&mut app), None, "a walking beast sat down");
        assert_eq!(picked(&app), None, "the row kept a sit the beast refused");
    }

    #[test]
    fn hold_poses_a_state_where_the_slider_puts_it_and_lets_go_blended() {
        // #53. Under hold the slider poses the way down itself, as the
        // driver's own clip is played - even at nought, where a rest asked
        // for none of itself poses nothing and would leave the driver's sit,
        // going on underneath, showing - and letting go of hold blends into
        // the sit the driver holds rather than jumping to it.
        let (mut app, rig, standing) = sat(1.0);
        let held = hips(&mut app, &rig);
        let at = |app: &mut App, through: f32| {
            {
                let mut animator = app.world_mut().resource_mut::<Animator>();
                animator.scrub = true;
                animator.gesture = Some(("Sit".to_string(), through));
            }
            for _ in 0..20 {
                tick(app);
            }
            hips(app, &rig)
        };
        let (nought, half, whole) = (at(&mut app, 0.0), at(&mut app, 0.5), at(&mut app, 1.0));
        assert!(
            (nought - standing).abs() < 0.01,
            "held at nought, the hips stood {:.0} mm off standing",
            (nought - standing) * 1000.0
        );
        assert!(
            half < nought - 0.02 && half > whole + 0.02,
            "half way read {half}"
        );
        assert!(
            (whole - held).abs() < 2.0e-3,
            "held at one, the hips stood {:.1} mm off the driver's sit",
            (whole - held) * 1000.0
        );
        at(&mut app, 0.0);
        app.world_mut().resource_mut::<Animator>().scrub = false;
        let worst = worst_step(&mut app, &rig, 30);
        // The blend's whole sit from nought, in its 0.15 s: its worst frame
        // moves a joint 153 mm, about a third of the hips' 0.42 m of travel,
        // and a jump would move it all in one.
        assert!(
            worst < 0.16,
            "letting go of hold moved a joint {:.0} mm in one frame",
            worst * 1000.0
        );
        assert!(
            (hips(&mut app, &rig) - held).abs() < 2.0e-3,
            "let go of, the slider did not hand the beast back to its sit"
        );
    }

    #[test]
    fn the_gaze_comes_back_blended_when_a_state_ends() {
        // #53. A state aims the head, so the window's gaze stands aside for
        // it, and whether it does is read before the driver's frame: on the
        // frame the beast is up the head stood where the body had it, and
        // the next frame's gaze turned it in one - or, with nothing else
        // moving and no blend to wait out, never turned it back at all. Held
        // off to the side, so the turn is all of the slider's.
        for blend in [Animator::default().blend, 0.0] {
            let (mut app, rig, _) = sat(1.0);
            {
                let mut animator = app.world_mut().resource_mut::<Animator>();
                animator.gaze_angle = 0.6;
                animator.blend = blend;
            }
            let head = *rig.in_zone(Zone::Head).first().expect("a head");
            let yaw = |app: &mut App| {
                let out = posed(app).forward(&rig).rotations[head]
                    * symbios_avatar::rig::landmark::FORWARD;
                out.x.atan2(out.z)
            };
            for _ in 0..5 {
                tick(&mut app);
            }
            let sitting = yaw(&mut app);
            assert!(
                sitting.abs() < 0.05,
                "blend {blend}: the window turned a sitting beast's head {sitting:.2} rad"
            );
            let up = emote::getting_up(&rig, "Sit")
                .expect("a sit to rise from")
                .1;
            pick(&mut app, None);
            let mut last = sitting;
            let mut worst = 0.0f32;
            for _ in 0..frames(up + 1.0) {
                tick(&mut app);
                let now = yaw(&mut app);
                worst = worst.max((now - last).abs());
                last = now;
            }
            assert!(
                last > 0.2,
                "blend {blend}: up, the head did not turn to the slider: {last:.2} rad"
            );
            assert!(
                blend <= 0.0 || worst < 0.3,
                "the head turned {worst:.2} rad in one frame as the beast got up"
            );
        }
    }

    #[test]
    fn a_state_picked_before_its_beast_is_built_is_asked_of_it_once_it_is() {
        // #53. The pick is asked of a driver on the frame it changes, so a
        // frame with no body to ask must not count as telling one: the
        // viewer's `--gesture Sit` is picked before anything is built. In a
        // window with nothing else moving, so the ask cannot ride a blink:
        // a new body writes nothing to the window to wake on.
        let mut app = bare_app();
        {
            let mut animator = app.world_mut().resource_mut::<Animator>();
            animator.blinking = false;
            animator.tracking = false;
        }
        pick(&mut app, Some("Sit"));
        tick(&mut app);
        app.world_mut().spawn(SpawnAvatar::from(AvatarRecord::new(
            "Late",
            Archetype::Quadruped(symbios_avatar::QuadrupedParams::default()),
        )));
        tick(&mut app);
        tick(&mut app);
        assert_eq!(
            playing(&mut app).map(|(name, _)| name).as_deref(),
            Some("Sit"),
            "a beast built after its sit was picked never sat"
        );
    }

    #[test]
    fn a_persons_gesture_runs_on_the_windows_clock_and_not_the_drivers() {
        // #53. Only a beast's states are its driver's: a person's gesture is
        // played by this window over its own second and a half, scrubbable,
        // as before.
        let mut app = app();
        pick(&mut app, Some("Greeting"));
        for _ in 0..45 {
            tick(&mut app);
        }
        assert_eq!(
            playing(&mut app),
            None,
            "a person's wave went to the driver"
        );
        let through = picked(&app).expect("still picked").1;
        assert!(
            (through - 0.5).abs() < 0.02,
            "three quarters of a second into a wave the slider read {through}"
        );
    }

    /// The default dog's archetype.
    fn dog() -> Archetype {
        Archetype::Quadruped(symbios_avatar::QuadrupedParams::default())
    }

    /// Despawns every body and asks for `record` in their place, as the
    /// record editor does on every edit, re-roll and change of archetype.
    fn rebuild_as(app: &mut App, record: AvatarRecord) {
        let mut bodies = app.world_mut().query_filtered::<Entity, With<AvatarBody>>();
        let old: Vec<Entity> = bodies.iter(app.world()).collect();
        for body in old {
            app.world_mut().despawn(body);
        }
        app.world_mut().spawn(SpawnAvatar::from(record));
    }

    /// How far below its bind pose the one body's hips are drawn: a beast
    /// down in a state has them well below, standing they are not.
    fn hips_down(app: &mut App) -> f32 {
        let rig = rig_of(app);
        let pelvis = *rig.in_zone(Zone::Pelvis).first().expect("a pelvis");
        rig.joints[pelvis].position.y - hips(app, &rig)
    }

    #[test]
    fn a_sitting_beast_rebuilt_rerolled_or_reshaped_stays_down_and_keeps_its_pick() {
        // #53. The record editor despawns and rebuilds the body on every
        // edit, and the window told its pick once for every body: the new
        // one's driver was never asked to sit, so it stood, and the row let
        // go of a sit it was not in. Each body is asked for itself, and one
        // built under a state the slider has it down in is drawn down there
        // from its first frame, so a sitting body can be tuned as it sits.
        let mut rerolled = AvatarRecord::new("Driven", dog());
        rerolled.reroll(11);
        let horse = AvatarRecord::new(
            "Driven",
            Archetype::Quadruped(symbios_avatar::QuadrupedParams {
                family: symbios_avatar::Family::Horse,
                ..symbios_avatar::QuadrupedParams::default()
            }),
        );
        for (what, record) in [
            ("rebuilt", AvatarRecord::new("Driven", dog())),
            ("rerolled", rerolled),
            ("a horse", horse),
        ] {
            let (mut app, rig, _) = sat(1.0);
            let held = hips(&mut app, &rig);
            rebuild_as(&mut app, record);
            tick(&mut app);
            let first = hips_down(&mut app);
            if what == "rebuilt" {
                let drawn = hips(&mut app, &rig);
                assert!(
                    (drawn - held).abs() < 2.0e-3,
                    "{what}: on its first frame the hips stood {:.1} mm off the sit",
                    (drawn - held) * 1000.0
                );
            }
            for _ in 0..frames(2.0) {
                tick(&mut app);
            }
            assert!(
                first > 0.1 && (hips_down(&mut app) - first).abs() < 2.0e-3,
                "{what}: the hips came {first:.3} m down on the first frame and \
                 then moved to {:.3}",
                hips_down(&mut app)
            );
            assert_eq!(
                playing(&mut app).map(|(name, _)| name).as_deref(),
                Some("Sit"),
                "{what}: the new body is not sitting"
            );
            assert_eq!(
                picked(&app),
                Some(("Sit".to_string(), 1.0)),
                "{what}: the row let go of the sit"
            );
        }
    }

    #[test]
    fn a_second_beast_spawned_while_one_sits_leaves_it_sitting() {
        // #53. One window-wide record of what the drivers were told made a
        // second body's fresh driver look like a state the beast had left, so
        // the row let go of the sit and the first beast stood up.
        let (mut app, rig, _) = sat(1.0);
        let held = hips(&mut app, &rig);
        let mut bodies = app.world_mut().query_filtered::<Entity, With<AvatarBody>>();
        let first = bodies.single(app.world()).expect("one body");
        app.world_mut()
            .spawn(SpawnAvatar::from(AvatarRecord::new("Second", dog())));
        for _ in 0..frames(1.0) {
            tick(&mut app);
        }
        let mut drivers = app.world_mut().query::<&AvatarDriver>();
        let sitting = drivers
            .iter(app.world())
            .filter(|driver| driver.gesture().is_some_and(|(name, _)| name == "Sit"))
            .count();
        assert_eq!(sitting, 2, "both beasts should be sitting");
        assert_eq!(
            picked(&app),
            Some(("Sit".to_string(), 1.0)),
            "the row let go"
        );
        let pose = app
            .world()
            .get::<AvatarPose>(first)
            .expect("the first body is posed")
            .0
            .clone();
        let pelvis = *rig.in_zone(Zone::Pelvis).first().expect("a pelvis");
        let drawn = pose.forward(&rig).positions[pelvis].y;
        assert!(
            (drawn - held).abs() < 2.0e-3,
            "the first beast's hips moved {:.1} mm",
            (drawn - held) * 1000.0
        );
    }

    #[test]
    fn letting_go_of_a_held_state_rises_from_where_the_slider_has_it() {
        // #53. Under hold the slider poses the way down while the driver's
        // own sit goes on underneath, and letting go of the pick - or asking
        // "Get Up" - got the beast up from that sit: from the slider's 0.3,
        // nearly standing, the blend carried the hips down to the full sit
        // and the beast rose from there. It rises from where the slider has
        // it, as a beast stopped part way down comes back up the way it went.
        for (route, at) in [(None, 0.3), (Some(emote::GET_UP), 0.3), (None, 1.0)] {
            let (mut app, rig, standing) = sat(1.0);
            {
                let mut animator = app.world_mut().resource_mut::<Animator>();
                animator.scrub = true;
                animator.gesture = Some(("Sit".to_string(), at));
            }
            for _ in 0..20 {
                tick(&mut app);
            }
            let posed_at = hips(&mut app, &rig);
            pick(&mut app, route);
            let up = emote::getting_up(&rig, "Sit")
                .expect("a sit to rise from")
                .1;
            let (mut lowest, mut last, mut worst) = (posed_at, drawn(&mut app, &rig), 0.0f32);
            for _ in 0..frames(up + 0.5) {
                tick(&mut app);
                lowest = lowest.min(hips(&mut app, &rig));
                let now = drawn(&mut app, &rig);
                let step = last
                    .iter()
                    .zip(&now)
                    .map(|(a, b)| a.distance(*b))
                    .fold(0.0f32, f32::max);
                worst = worst.max(step);
                last = now;
            }
            assert!(
                lowest > posed_at - 5.0e-3,
                "{route:?} at {at}: from {posed_at:.3} the hips went down to {lowest:.3} \
                 before rising"
            );
            // The beast's own way up, from part way down or from its sit,
            // moves its fastest joint 34 and 52 mm a frame, which is the
            // engine's to answer for: the bound is for the window's part,
            // which adds nothing to it.
            assert!(
                worst < 0.055,
                "{route:?} at {at}: a joint moved {:.0} mm in one frame",
                worst * 1000.0
            );
            assert_eq!(
                playing(&mut app),
                None,
                "{route:?} at {at}: it never got up"
            );
            assert!(
                (hips(&mut app, &rig) - standing).abs() < 0.01,
                "{route:?} at {at}: up, the hips stood {:.0} mm off where they began",
                (hips(&mut app, &rig) - standing) * 1000.0
            );
        }
    }

    #[test]
    fn the_slider_reads_the_way_down_only_while_the_body_is_on_it() {
        // #53. The readout was the driver's count over the way down's time,
        // whatever the driver was doing: a sit picked again while the beast
        // was still getting up from the last read one all the way up, then
        // dropped, and a beast walked off part way down read its way down
        // climbing while it rose. It follows the pick's own way down and
        // stays where it was otherwise, so it never runs back nor jumps.
        let (mut app, rig, _) = sat(1.0);
        let down = emote::seconds(&rig, "Sit").expect("a beast sits");
        // One frame's share of the way down, and a little for the rounding.
        let most = 1.0 / 60.0 / down + 1.0e-4;
        pick(&mut app, None);
        for _ in 0..20 {
            tick(&mut app);
        }
        pick(&mut app, Some("Sit"));
        let mut last = 0.0f32;
        for _ in 0..frames(3.0) {
            tick(&mut app);
            let through = picked(&app).expect("still picked").1;
            assert!(
                through >= last && through - last <= most,
                "picked again while rising, the slider went from {last} to {through}"
            );
            last = through;
        }
        assert!((last - 1.0).abs() < 1.0e-6, "the sit read {last} once down");

        let mut app = beast_app();
        tick(&mut app);
        pick(&mut app, Some("Sit"));
        for _ in 0..30 {
            tick(&mut app);
        }
        let before = picked(&app).expect("still picked").1;
        app.world_mut().resource_mut::<Animator>().walking = true;
        while let Some((_, through)) = picked(&app) {
            assert!(
                (through - before).abs() < 1.0e-6,
                "walked part way down, the slider went from {before} to {through}"
            );
            tick(&mut app);
        }
    }

    #[test]
    fn hold_never_poses_a_state_on_a_beast_that_will_not_rest() {
        // #53. Under hold, a sit picked on a walking beast slipped past the
        // let-go - the driver refused it, and a scrub was exempt - and the
        // row held it at its end, posing a full sit over the walk. The beast
        // is drawn exactly as one walking with nothing picked, from the
        // first frame, and the row lets go.
        let walking = || {
            let mut app = beast_app();
            {
                let mut animator = app.world_mut().resource_mut::<Animator>();
                animator.walking = true;
                animator.scrub = true;
            }
            tick(&mut app);
            app
        };
        let (mut app, mut plain) = (walking(), walking());
        pick(&mut app, Some("Sit"));
        for _ in 0..10 {
            tick(&mut app);
            tick(&mut plain);
            assert!(
                posed(&mut app) == posed(&mut plain),
                "a sit was posed over a walking beast"
            );
        }
        assert_eq!(picked(&app), None, "the row kept a sit the beast refused");
    }

    /// The text one frame of the motion window's locomotion section shows
    /// for `animator`, read through AccessKit.
    #[cfg(feature = "editor")]
    fn offered(animator: &mut Animator, legs: usize, states: &[&str]) -> Vec<String> {
        use bevy_egui::egui;
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        let clips = Clips::default();
        let output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(screen),
                ..Default::default()
            },
            |ui| locomotion_section(ui, &clips, animator, legs, states),
        );
        output
            .platform_output
            .accesskit_update
            .expect("AccessKit is on")
            .nodes
            .iter()
            .filter_map(|(_, node)| node.label().or_else(|| node.value()).map(str::to_owned))
            .collect()
    }

    #[cfg(feature = "editor")]
    #[test]
    fn hold_left_on_is_offered_wherever_it_freezes_a_gesture() {
        // #53. The state's hold is drawn only while a state is picked, and
        // the phase row's only while the body is not standing, so a hold
        // turned on for a sit and left on as the sit was let go of was drawn
        // nowhere: every later gesture stood frozen at nought - a wag, and
        // after a rebuild a person's wave - with nothing to turn it off.
        let (mut app, rig, _) = sat(0.2);
        let states: Vec<&str> = emote::ROSTER
            .iter()
            .copied()
            .filter(|name| state(&rig, name))
            .collect();
        app.world_mut().resource_mut::<Animator>().scrub = true;
        for _ in 0..5 {
            tick(&mut app);
        }
        pick(&mut app, None);
        for _ in 0..frames(2.0) {
            tick(&mut app);
        }
        let mut animator = std::mem::take(&mut *app.world_mut().resource_mut::<Animator>());
        assert!(animator.scrub, "the hold was let go of on its own");
        assert!(
            offered(&mut animator, 4, &states)
                .iter()
                .any(|label| label == "hold"),
            "a hold left on over a standing beast was drawn nowhere"
        );
        animator.gesture = Some(("Wag".to_string(), 0.0));
        assert!(
            offered(&mut animator, 4, &states)
                .iter()
                .any(|label| label == "hold"),
            "a wag frozen under hold was offered no hold"
        );
        animator.gesture = None;
        *app.world_mut().resource_mut::<Animator>() = animator;
        rebuild_as(&mut app, AvatarRecord::new("Person", Archetype::default()));
        tick(&mut app);
        pick(&mut app, Some("Greeting"));
        tick(&mut app);
        let mut animator = std::mem::take(&mut *app.world_mut().resource_mut::<Animator>());
        assert!(
            offered(&mut animator, 2, &[])
                .iter()
                .any(|label| label == "hold"),
            "a person's wave frozen under hold was offered no hold"
        );
    }

    #[cfg(feature = "editor")]
    #[test]
    fn a_gesture_the_beast_is_playing_stays_lit_until_it_ends() {
        // #53. A shake picked while the beast sits is the driver's, played
        // once it is up, and once started the engine plays it to its end:
        // clicked off, the row went dark over a beast still shaking. It
        // stays lit until it ends. Clicked off while the beast is still
        // getting up, before it plays, it is dropped and never plays.
        let click = |app: &mut App, name: &str| {
            click_gesture(&mut app.world_mut().resource_mut::<Animator>(), name);
        };
        let (mut app, rig, _) = sat(1.0);
        let up = emote::getting_up(&rig, "Sit")
            .expect("a sit to rise from")
            .1;
        click(&mut app, "Shake");
        let mut waited = 0;
        while playing(&mut app).is_none_or(|(name, _)| name != "Shake") {
            assert!(waited < frames(up + 0.5), "the beast never shook");
            tick(&mut app);
            waited += 1;
        }
        tick(&mut app);
        click(&mut app, "Shake");
        assert_eq!(
            picked(&app).map(|(name, _)| name).as_deref(),
            Some("Shake"),
            "the row went dark over a beast still shaking"
        );
        for _ in 0..frames(GESTURE_TIME) {
            tick(&mut app);
        }
        assert_eq!(playing(&mut app), None, "the shake never ended");
        assert_eq!(
            picked(&app),
            Some(("Shake".to_string(), 1.0)),
            "held at its end"
        );
        click(&mut app, "Shake");
        assert_eq!(
            picked(&app),
            None,
            "a shake that has ended was not let go of"
        );

        let (mut app, _, _) = sat(1.0);
        click(&mut app, "Shake");
        for _ in 0..5 {
            tick(&mut app);
        }
        click(&mut app, "Shake");
        assert_eq!(picked(&app), None, "a shake not yet playing was kept");
        for _ in 0..frames(up + GESTURE_TIME) {
            tick(&mut app);
            assert!(
                playing(&mut app).is_none_or(|(name, _)| name != "Shake"),
                "a shake let go of before it played played anyway"
            );
        }
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
