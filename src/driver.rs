//! Driving a body per entity, from whatever is carrying it.
//!
//! The engine owns the motion and the decisions — which stage is running this
//! frame, the clocks that outlive a frame, the joins between them — in
//! [`symbios_avatar::anim::driver`]. This module is the thin Bevy shape around
//! it: a driver per body in [`AvatarDriver`], the frame's facts in [`Drive`],
//! and one system that carries the second into the first and writes the result
//! onto the entity.
//!
//! # The two ways to move a body in this crate
//!
//! [`AnimatorPlugin`](crate::AnimatorPlugin) drives every body from **one
//! resource**, which is what a viewer wants: there is one subject and the
//! question is always "what is it doing now". This drives **each body from its
//! own component**, which is what an application wants: a room of avatars, each
//! carried by its own chassis, each doing something different.
//!
//! They do not fight. The animator stands aside for any body that carries an
//! [`AvatarDriver`], so a consumer can add both plugins and drive some bodies
//! from the panel and others from their chassis.
//!
//! # Using it
//!
//! Put an [`AvatarDriver`] and a [`Drive`] on a body, write the [`Drive`] each
//! frame from whatever moves the body, and order that system before
//! [`drive_avatar_bodies`]:
//!
//! ```no_run
//! use bevy::prelude::*;
//! use bevy_symbios_avatar::{
//!     AvatarDriver, AvatarPlugin, AvatarSystems, Drive, drive_avatar_bodies,
//! };
//!
//! fn follow_the_chassis(time: Res<Time>, mut bodies: Query<(&GlobalTransform, &mut Drive)>) {
//!     for (chassis, mut drive) in &mut bodies {
//!         drive.moved_to(chassis.translation(), time.delta_secs());
//!     }
//! }
//!
//! App::new().add_plugins((DefaultPlugins, AvatarPlugin)).add_systems(
//!     Update,
//!     follow_the_chassis
//!         .before(drive_avatar_bodies)
//!         .in_set(AvatarSystems::Animate),
//! );
//! ```
//!
//! A body with an [`AvatarDriver`] and **no** [`Drive`] is left entirely alone,
//! which is how a consumer that wants to call
//! [`Driver::drive`](symbios_avatar::anim::Driver::drive) itself opts out — see
//! [`drive_avatar_bodies`] for the two reasons to.

use bevy::prelude::*;
use symbios_avatar::anim::Pattern;
use symbios_avatar::anim::driver::{
    Driven, Driver, DriverConfig, Hold, Inputs, Showing, Source, WalkFlags, level_ground,
    velocity_of,
};
use symbios_avatar::{Expression, Gait, Heading};

use crate::ground::GroundPatch;
use crate::spawn::{AvatarBody, AvatarClosure, AvatarPose};

/// One body's motion state: every clock that outlives a frame.
///
/// **Seeded per body, and there is deliberately no [`Default`].** An idle's
/// seed decides when its settling weight shift fires and which leg it moves
/// first, so a room of bodies sharing a seed breathes, shifts its weight and
/// blinks in unison and reads as a drill team rather than a crowd. A seed drawn
/// from somewhere the caller cannot see is worse still: it makes any
/// measurement of the body a function of how many bodies were built before it.
#[derive(Component, Deref, DerefMut)]
pub struct AvatarDriver(pub Driver);

impl AvatarDriver {
    /// A driver whose idle and blink are seeded from `seed`.
    #[must_use]
    pub fn seeded(seed: u64) -> Self {
        Self(Driver::seeded(seed))
    }

    /// The same, tuned by something other than the defaults — which is where
    /// [`Carriage`](symbios_avatar::anim::Carriage) is chosen, and it has no
    /// default that suits both a body moved by a physics engine and one that
    /// carries itself.
    #[must_use]
    pub fn new(config: DriverConfig, seed: u64) -> Self {
        Self(Driver::new(config, seed))
    }
}

/// What is happening to one body this frame, as its owner sees it.
///
/// Written by whatever moves the body and read by [`drive_avatar_bodies`]. It
/// is [`Inputs`] in component form, minus the frame's own length — which comes
/// from [`Time`] — and minus the two borrowed layers, which a component cannot
/// hold; see [`drive_avatar_bodies`]. The ground the engine takes as a closure
/// it takes here as data, [`Self::ground`] (#56).
///
/// Every field past the first three has a default that means "nothing
/// unusual", so an application fills in a place and a velocity and leaves the
/// rest.
#[derive(Component, Clone, Debug)]
pub struct Drive {
    /// How fast the body is travelling, in world metres per second.
    ///
    /// The horizontal magnitude picks the gait and its speed; the **signed**
    /// vertical is the whole of the airborne state machine. [`Self::moved_to`]
    /// derives one for a body whose motion arrives as positions.
    pub velocity: Vec3,
    /// Where the body is in the world. Only its horizontal is read.
    pub at: Vec3,
    /// The yaw about `+Y` carrying the body's own `+Z` onto its world heading.
    ///
    /// **Not the carrying entity's rotation**, where the two differ. A consumer
    /// whose body hangs off a root that corrects between its own forward
    /// convention and the engine's must carry that correction into this angle,
    /// or every held foothold is mirrored through the body and a walking body
    /// reads as skating.
    pub facing: f32,
    /// Which way the body travels relative to the way it faces, or [`None`] for
    /// straight ahead.
    pub heading: Option<Heading>,
    /// Whether the body is in water deep enough to swim in. The consumer's
    /// classification, because what counts as deep is a fact about the world
    /// rather than about the body.
    pub swimming: bool,
    /// A gesture asked for, by the name the engine's roster knows it as.
    ///
    /// **Taken by the drive**, so setting it is a request that fires once and a
    /// consumer never has to remember to clear it. A name the roster does not
    /// carry is ignored and spends no cooldown.
    ///
    /// **On four legs, Sit and Lie Down are states** (engine #377): asked for
    /// once, the beast goes down at its own pace and stays down until
    /// something gets it up - its being moved, leaving the ground or
    /// swimming, which gets it up in a hurry; another gesture, which it gets
    /// up for and then plays; or the engine's `emote::GET_UP`, "Get Up", which
    /// gets it up where it is. The motion window plays a beast's states
    /// through this same driver (#53), so what it shows is what this gets.
    pub gesture: Option<String>,
    /// Whether an editor is holding the body, and how.
    pub hold: Hold,
    /// A motion to show instead of the one the chassis implies.
    pub showing: Option<Showing>,
    /// Hold the cycle at this point instead of running it.
    pub cycle: Option<f32>,
    /// Run the cycle at this rate instead of the one the speed implies.
    pub cadence: Option<f32>,
    /// Walk in this pattern instead of the one the speed picks.
    pub gait: Option<Gait>,
    /// On four legs, move in this pattern - a walk, a trot, a canter, a
    /// gallop - instead of the one the speed picks, its cycle still the
    /// speed's and its duty the pattern's (#370): see the engine's
    /// `Inputs::pattern`. A body that is not four-legged ignores it.
    pub pattern: Option<Pattern>,
    /// The walk's ablation switches.
    pub walk: WalkFlags,
    /// How fast the body is turning, in radians a second, positive toward its
    /// own left.
    pub turn: f32,
    /// Hold the lids at this point of a blink instead of blinking.
    pub lids: Option<f32>,
    /// The face the body is resting in, which decides where the lids sit when
    /// they are not mid-blink.
    pub face: Expression,
    /// The ground under the body this frame, in its own frame, or [`None`] for
    /// the level floor at the root's `y = 0` (#56).
    ///
    /// **Data rather than the closure the engine takes**, because a closure is
    /// a borrow of whatever it reads and a component cannot hold one: the
    /// owner samples its world into a [`GroundPatch`] and the drive answers the
    /// engine from that. Rewrite it every frame the body moves, as the rest of
    /// this component is - a patch is the ground where the body WAS when it was
    /// sampled, and the drive reads it as the ground where the body is.
    /// [`GroundPatch::resample`] keeps the allocation for that.
    ///
    /// [`None`] is exactly the floor this component always had: a body
    /// without a patch is driven bit for bit as it was before there was one.
    pub ground: Option<GroundPatch>,
}

impl Default for Drive {
    /// A body standing still at the origin, facing down its own `+Z`, with
    /// nothing overridden.
    fn default() -> Self {
        Self {
            velocity: Vec3::ZERO,
            at: Vec3::ZERO,
            facing: 0.0,
            heading: None,
            swimming: false,
            gesture: None,
            hold: Hold::None,
            showing: None,
            cycle: None,
            cadence: None,
            gait: None,
            pattern: None,
            walk: WalkFlags::default(),
            turn: 0.0,
            lids: None,
            face: Expression::NEUTRAL,
            ground: None,
        }
    }
}

impl Drive {
    /// A body standing at a place.
    #[must_use]
    pub fn standing(at: Vec3) -> Self {
        Self {
            at,
            ..Self::default()
        }
    }

    /// A body at a place, travelling.
    #[must_use]
    pub fn travelling(at: Vec3, velocity: Vec3) -> Self {
        Self {
            velocity,
            at,
            ..Self::default()
        }
    }

    /// Move the body to `place`, working its velocity out from where it was.
    ///
    /// **For a body whose motion arrives as positions rather than as a
    /// velocity** — a remote peer played back from what the network said, or
    /// anything driven by a transform somebody else writes. The previous
    /// position is [`Self::at`] itself, so this is the whole of what a consumer
    /// on that path has to call and there is no second copy of the position to
    /// keep in step.
    ///
    /// A frame with no time in it leaves the velocity at zero rather than at an
    /// infinity, which would read as a launch.
    pub fn moved_to(&mut self, place: Vec3, delta: f32) {
        self.velocity = velocity_of(self.at, place, delta);
        self.at = place;
    }

    /// Ask for a gesture by the name the engine's roster knows it as.
    pub fn gesture(&mut self, name: impl Into<String>) {
        self.gesture = Some(name.into());
    }
}

/// What the last driven frame produced, for anything that wants to ask.
///
/// The pose and the closure land on the body as [`AvatarPose`] and
/// [`AvatarClosure`], because those are what draws it. This is the rest of the
/// answer — the part a consumer reads rather than draws.
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct Drove {
    /// What carried the body.
    pub source: Source,
    /// Whether any contact's goal was out of the solver's reach.
    ///
    /// A body that strains occasionally is a body on hard ground; one that
    /// strains constantly is a body whose goals are wrong.
    pub strained: bool,
    /// Whether a gesture aimed the head, so a consumer's own gaze layer should
    /// stand aside.
    pub aimed: bool,
}

impl From<&Driven> for Drove {
    fn from(driven: &Driven) -> Self {
        Self {
            source: driven.source,
            strained: driven.strained,
            aimed: driven.aimed,
        }
    }
}

/// Drives every body that carries an [`AvatarDriver`] and a [`Drive`].
///
/// Runs in [`AvatarSystems::Animate`](crate::AvatarSystems::Animate): after
/// bodies are built and destroyed, before poses are applied. A body rebuilt
/// this frame is posed this frame, and one destroyed this frame is not posed at
/// all.
///
/// Nothing is written for a frame the driver declined: a frame with no time in
/// it, or a body held exactly where it stands. The last pose stays applied in
/// both cases, which is the whole point of the second — a hold is a pause, not
/// a re-pose.
///
/// # The ground under the feet
///
/// A body's feet are planted on [`Drive::ground`] when it carries one, and on
/// the **level floor** at its root's `y = 0` ([`level_ground`]) when it does
/// not (#56). The patch is data the owner samples from its world, because the
/// engine's ground is a closure and no closure can live in a component; this
/// system answers the engine from it. A body with no patch is handed
/// [`level_ground`] itself, exactly as before there was a patch to carry.
///
/// # When to drive a body yourself instead
///
/// This system lays **no layer** over the motion, because an overlay cannot
/// live in a component: it is a borrow of something a system holds. A consumer
/// that needs one - an authored clip or a face over the pose, or a ground it
/// can only answer live, such as a physics ray per probe - writes its own
/// system that builds [`Inputs`] with those closures and calls
/// [`Driver::drive`](symbios_avatar::anim::Driver::drive) directly. That is a
/// dozen lines and no duplicated state machine, which is the division this
/// module exists to make possible. Such a body carries an [`AvatarDriver`] and
/// no [`Drive`], and this system passes it by.
pub fn drive_avatar_bodies(
    mut commands: Commands,
    time: Res<Time>,
    mut bodies: Query<(Entity, &AvatarBody, &mut AvatarDriver, &mut Drive)>,
) {
    let delta = time.delta_secs();
    for (entity, body, mut driver, mut drive) in &mut bodies {
        // Taken rather than read, so a request fires once and a consumer never
        // has to remember to clear it.
        let gesture = drive.gesture.take();
        let drive = &*drive;
        let inputs = inputs_of(drive, gesture.as_deref(), body, delta);
        // The two arms are two monomorphisations rather than one closure that
        // checks for a patch, so a body without one is handed the engine's own
        // `level_ground` and is driven exactly as it was before #56.
        let driven = match &drive.ground {
            Some(patch) => driver.drive(&body.avatar.rig, &inputs, |point| patch.ground(point)),
            None => driver.drive(&body.avatar.rig, &inputs, level_ground),
        };
        let Some(driven) = driven else {
            continue;
        };
        // `try_insert`, because a body may leave in the frame it is driven: a
        // despawn queued ahead of this one lands first in the flush, and a plain
        // `insert` on a despawned entity panics (#54).
        commands.entity(entity).try_insert((
            Drove::from(&driven),
            AvatarPose(driven.pose),
            AvatarClosure(driven.closure),
        ));
    }
}

/// One body's [`Inputs`] for a frame `delta` long, from its [`Drive`] and the
/// `gesture` taken from it.
///
/// Its own function so the tests can build exactly what the system hands the
/// driver, and drive a second driver from it by hand.
fn inputs_of<'a>(
    drive: &'a Drive,
    gesture: Option<&'a str>,
    body: &'a AvatarBody,
    delta: f32,
) -> Inputs<'a> {
    Inputs {
        delta,
        velocity: drive.velocity,
        at: drive.at,
        facing: drive.facing,
        heading: drive.heading,
        swimming: drive.swimming,
        gesture,
        hold: drive.hold,
        showing: drive.showing,
        cycle: drive.cycle,
        cadence: drive.cadence,
        gait: drive.gait.as_ref(),
        pattern: drive.pattern,
        walk: drive.walk,
        turn: drive.turn,
        lids: drive.lids,
        face: drive.face,
        // Handed over here rather than left to the caller, so the rule that
        // a blink rides AFTER the blend cannot be forgotten by a consumer:
        // smoothed by a gait transition, a blink arrives as a slow
        // heavy-lidded droop that reads as a body falling asleep.
        eyes: body.avatar.parts.eyes.as_ref(),
        over_locomotion: None,
        over_settled: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::mesh::skinning::SkinnedMeshInverseBindposes;
    use symbios_avatar::{Archetype, AvatarRecord};

    /// A headless app with just enough of Bevy to build a body and drive it.
    ///
    /// **Without `TimePlugin`, deliberately.** A driven frame is a function of
    /// how long it was, and left to the real clock a test frame is a few
    /// microseconds — so a body would breathe a thousandth of a breath over
    /// what the test calls a second. Disabling the plugin leaves [`Time`] to
    /// [`tick`], which is what makes these frames a fixed sixtieth of a second
    /// and the results the same on a fast machine and a slow one.
    fn app() -> App {
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins.build().disable::<bevy::time::TimePlugin>(),
            AssetPlugin::default(),
            bevy::mesh::MeshPlugin,
            bevy::image::ImagePlugin::default(),
            crate::AvatarPlugin,
        ))
        .init_resource::<Time>()
        .init_asset::<StandardMaterial>()
        .init_asset::<SkinnedMeshInverseBindposes>();
        app
    }

    /// Spawns a body that drives itself, and runs the frame that builds it.
    ///
    /// **The driver goes on at spawn, not afterwards.** A body is built and
    /// posed in one frame, so a body that spends its first frame without an
    /// [`AvatarDriver`] is a body the animator resource legitimately owned for
    /// that frame — which is a fact about the test's ordering rather than about
    /// either system, and it read as a defect once.
    fn spawn_driven(app: &mut App) -> Entity {
        let entity = app
            .world_mut()
            .spawn((
                crate::SpawnAvatar::from(AvatarRecord::new("Driven", Archetype::default())),
                AvatarDriver::seeded(7),
                Drive::default(),
            ))
            .id();
        tick(app);
        entity
    }

    /// Spawns a body with a driver and no [`Drive`] — the opt-out.
    fn spawn_undriven(app: &mut App) -> Entity {
        let entity = app
            .world_mut()
            .spawn((
                crate::SpawnAvatar::from(AvatarRecord::new("Driven", Archetype::default())),
                AvatarDriver::seeded(7),
            ))
            .id();
        tick(app);
        entity
    }

    /// One frame, a sixtieth of a second long.
    fn tick(app: &mut App) {
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_secs_f32(1.0 / 60.0));
        app.update();
    }

    /// A body that leaves in the frame it is driven takes the frame's pose with
    /// it (#54): a despawn ahead of the drive's own commands lands first in the
    /// flush, as a peer's departure does, and the pose finds nothing to land on.
    ///
    /// **In the drive's set and `before_ignore_deferred`, or this proves nothing:**
    /// a plain `before` puts a sync point between the two, and a system outside the
    /// set is flushed at the sync point ahead of it. Either way the despawn is
    /// applied before the drive runs, and the drive's query never sees the body.
    #[test]
    fn a_body_despawned_in_the_frame_it_is_driven_is_let_go() {
        let mut app = app();
        let entity = spawn_driven(&mut app);
        app.add_systems(
            Update,
            (move |mut commands: Commands| commands.entity(entity).despawn())
                .in_set(crate::AvatarSystems::Animate)
                .before_ignore_deferred(drive_avatar_bodies),
        );
        tick(&mut app);
        assert!(
            app.world().get_entity(entity).is_err(),
            "the body should be gone"
        );
    }

    #[test]
    fn a_body_with_a_driver_and_a_drive_is_posed_every_frame() {
        let mut app = app();
        let body = spawn_driven(&mut app);

        let first = app
            .world()
            .get::<AvatarPose>(body)
            .expect("a driven body is posed")
            .0
            .clone();
        assert_eq!(
            app.world()
                .get::<Drove>(body)
                .expect("and reported on")
                .source,
            Source::Idle,
            "a body standing still is idling"
        );

        // A breath is slow, so this is not adjacent frames: a second apart, a
        // standing body must have moved.
        for _ in 0..60 {
            tick(&mut app);
        }
        let later = &app.world().get::<AvatarPose>(body).expect("still posed").0;
        let moved = (0..first.rotations.len())
            .map(|joint| 1.0 - first.rotations[joint].dot(later.rotations[joint]).abs())
            .fold(0.0f32, f32::max);
        assert!(
            moved > 1e-6,
            "a second of standing drew a bit-identical skeleton — the driver is not running"
        );
    }

    #[test]
    fn a_held_body_is_left_exactly_where_it_stands() {
        // The editing hold is a pause, not a re-pose: nothing is written, so
        // the last pose stays applied and the pose writer never sees a change.
        let mut app = app();
        let body = spawn_driven(&mut app);

        let held = app
            .world()
            .get::<AvatarPose>(body)
            .expect("posed")
            .0
            .clone();
        app.world_mut()
            .entity_mut(body)
            .get_mut::<Drive>()
            .expect("a drive")
            .hold = Hold::Pose;
        for _ in 0..30 {
            tick(&mut app);
        }
        let after = &app.world().get::<AvatarPose>(body).expect("still posed").0;
        assert_eq!(
            after.rotations, held.rotations,
            "a held body was re-posed — the hold moved the thing it exists to hold still"
        );
    }

    #[test]
    fn the_animator_stands_aside_for_a_body_that_drives_itself() {
        // Both plugins can be added at once, so the rule has to be enforced
        // rather than documented: two writers on one pose is a body flickering
        // between two motions.
        let mut app = app();
        app.add_plugins(crate::AnimatorPlugin);
        app.world_mut().resource_mut::<crate::Animator>().walking = true;
        let body = spawn_driven(&mut app);

        // The animator would be walking this body; the driver is standing it
        // still.
        assert_eq!(
            app.world().get::<Drove>(body).expect("driven").source,
            Source::Idle,
            "the driver did not run"
        );
        // And the window never reached it: its cursor is advanced by the first
        // body it steers, so a cursor still at zero is a window that steered
        // nothing. (`Blending` used to be the witness; since #43 the animator
        // has no per-body state left to look for.)
        assert_eq!(
            app.world().resource::<crate::Animator>().cycle.to_bits(),
            0.0_f32.to_bits(),
            "the animator steered a body that carries a Drive"
        );
    }

    #[test]
    fn a_body_with_no_drive_is_left_to_its_owner() {
        // The opt-out: a consumer calling the engine's driver itself, with its
        // own ground and its own layers, must not also be driven from here.
        let mut app = app();
        let body = spawn_undriven(&mut app);
        tick(&mut app);
        assert!(
            app.world().get::<Drove>(body).is_none(),
            "a body with no Drive was driven anyway"
        );
        assert!(
            app.world().get::<AvatarPose>(body).is_none(),
            "a body with no Drive was posed anyway"
        );
    }

    #[test]
    fn a_gesture_request_fires_once() {
        let mut app = app();
        let body = spawn_driven(&mut app);
        app.world_mut()
            .entity_mut(body)
            .get_mut::<Drive>()
            .expect("a drive")
            .gesture("Greeting");
        tick(&mut app);

        assert!(
            app.world()
                .get::<Drive>(body)
                .expect("a drive")
                .gesture
                .is_none(),
            "the request outlived the frame that answered it, so it will fire again"
        );
        let (name, _) = app
            .world()
            .get::<AvatarDriver>(body)
            .expect("a driver")
            .gesture()
            .expect("the greeting started");
        assert_eq!(name, "Greeting");
    }

    #[test]
    fn a_travelling_body_walks() {
        // The whole point of the component: a velocity written by whatever
        // moves the body reaches the gait without the consumer choosing one.
        let mut app = app();
        let body = spawn_driven(&mut app);
        let mut at = Vec3::ZERO;
        for _ in 0..30 {
            at += Vec3::Z * (1.4 / 60.0);
            app.world_mut()
                .entity_mut(body)
                .get_mut::<Drive>()
                .expect("a drive")
                .moved_to(at, 1.0 / 60.0);
            tick(&mut app);
        }
        assert_eq!(
            app.world().get::<Drove>(body).expect("driven").source,
            Source::Gait,
            "a body travelling at 1.4 m/s is walking"
        );
    }

    /// Spawns a body of `archetype` for each drive in `drives`, all seeded
    /// alike and built small, and runs the one frame that builds and first
    /// drives them all.
    ///
    /// A 64-texel atlas and no far hair tier, because these measure where the
    /// feet go and nothing drawn: the default atlas costs every test a paint
    /// it never looks at. One frame for all of them, so bodies compared frame
    /// by frame have been driven the same number of times.
    fn spawn_small(
        app: &mut App,
        archetype: &Archetype,
        seed: u64,
        drives: Vec<Drive>,
    ) -> Vec<Entity> {
        let bodies = drives
            .into_iter()
            .map(|drive| {
                app.world_mut()
                    .spawn((
                        crate::SpawnAvatar {
                            record: AvatarRecord::new("Grounded", archetype.clone()),
                            config: symbios_avatar::AvatarConfig {
                                atlas: 64,
                                ..symbios_avatar::AvatarConfig::default()
                            },
                            closure: 0.0,
                        },
                        AvatarDriver::seeded(seed),
                        drive,
                    ))
                    .id()
            })
            .collect();
        tick(app);
        bodies
    }

    fn beast() -> Archetype {
        Archetype::Quadruped(symbios_avatar::QuadrupedParams::default())
    }

    /// A pose as bits, so two poses compare equal only when every float in
    /// them is the same float: `-0.0 == 0.0` would pass a comparison of values.
    fn bits(pose: &symbios_avatar::Pose) -> (Vec<[u32; 4]>, [u32; 3]) {
        (
            pose.rotations
                .iter()
                .map(|rotation| rotation.to_array().map(f32::to_bits))
                .collect(),
            pose.translation.to_array().map(f32::to_bits),
        )
    }

    /// The pose last written onto `body`.
    fn posed(app: &App, body: Entity) -> symbios_avatar::Pose {
        app.world()
            .get::<AvatarPose>(body)
            .expect("a driven body is posed")
            .0
            .clone()
    }

    /// Walk `bodies` down their own `+Z` at 1.4 m/s for `frames` frames, then
    /// stand them still for as many, calling `each` after every frame.
    fn walk_then_stand(
        app: &mut App,
        bodies: &[Entity],
        frames: usize,
        mut each: impl FnMut(&mut App, usize),
    ) {
        let mut at = Vec3::ZERO;
        for frame in 0..frames * 2 {
            if frame < frames {
                at += Vec3::Z * (1.4 / 60.0);
            }
            for &body in bodies {
                app.world_mut()
                    .entity_mut(body)
                    .get_mut::<Drive>()
                    .expect("a drive")
                    .moved_to(at, 1.0 / 60.0);
            }
            tick(app);
            each(app, frame);
        }
    }

    /// A body without a patch is driven by exactly the call this system made
    /// before there were patches (#56): a second driver, seeded alike and
    /// handed the same inputs and the engine's own `level_ground` by hand,
    /// draws the same skeleton to the bit on every frame of a walk and a stop.
    #[test]
    fn a_body_without_a_patch_is_driven_exactly_as_on_the_level_floor() {
        let mut app = app();
        let [body] = spawn_small(&mut app, &beast(), 11, vec![Drive::default()])[..] else {
            unreachable!()
        };
        let mut by_hand = Driver::seeded(11);
        let mut check = |app: &mut App, frame: usize| {
            let world = app.world();
            let delta = world.resource::<Time>().delta_secs();
            let avatar = world.get::<AvatarBody>(body).expect("built");
            let drive = world.get::<Drive>(body).expect("a drive");
            assert!(drive.ground.is_none());
            let inputs = inputs_of(drive, None, avatar, delta);
            let driven = by_hand
                .drive(&avatar.avatar.rig, &inputs, level_ground)
                .expect("a frame with time in it is driven");
            assert_eq!(
                bits(&driven.pose),
                bits(&posed(app, body)),
                "frame {frame}: the system drove a body with no patch off some other floor"
            );
        };
        check(&mut app, 0);
        walk_then_stand(&mut app, &[body], 60, check);
    }

    /// A patch of level ground is the level floor (#56): two bodies seeded
    /// alike, one carrying a patch of zeros over its whole reach and one
    /// carrying none, draw the same skeleton to the bit on every frame of a
    /// walk and a stop.
    #[test]
    fn a_level_patch_drives_exactly_as_the_level_floor() {
        let mut app = app();
        let rig = symbios_avatar::Avatar::build_with(
            &AvatarRecord::new("Grounded", beast()),
            &symbios_avatar::AvatarConfig {
                atlas: 64,
                ..symbios_avatar::AvatarConfig::default()
            },
        )
        .expect("the beast builds")
        .rig;
        let reach = GroundPatch::reach(&rig).expect("a beast stands on its feet");
        let flat = Drive {
            ground: Some(GroundPatch::sampled(reach, 0.25, |_| 0.0)),
            ..Drive::default()
        };
        let [level, patched] = spawn_small(&mut app, &beast(), 5, vec![Drive::default(), flat])[..]
        else {
            unreachable!()
        };
        let same = |app: &App, frame: usize| {
            assert_eq!(
                bits(&posed(app, level)),
                bits(&posed(app, patched)),
                "frame {frame}: a patch of zeros drove the body off the level floor"
            );
        };
        same(&app, 0);
        walk_then_stand(&mut app, &[level, patched], 60, |app, frame| {
            same(app, frame);
        });
    }

    /// How far above the ground `height` gives each foot `body` stands on is
    /// in its last pose: the lowest extremity joint of every limb it stands on
    /// ([`Rig::ground_contacts`](symbios_avatar::Rig::ground_contacts)),
    /// whether that foot is down this frame or not.
    fn soles(app: &App, body: Entity, height: impl Fn(Vec3) -> f32) -> Vec<f32> {
        let rig = &app
            .world()
            .get::<AvatarBody>(body)
            .expect("built")
            .avatar
            .rig;
        let at = posed(app, body).forward(rig);
        rig.ground_contacts()
            .into_iter()
            .map(|limb| {
                let sole = rig
                    .extremity_joints(limb)
                    .into_iter()
                    .map(|joint| at.positions[joint])
                    .min_by(|a, b| a.y.total_cmp(&b.y))
                    .expect("a contact has an extremity");
                sole.y - height(sole)
            })
            .collect()
    }

    /// A beast on a slope stands on the slope (#56). Two beasts seeded alike
    /// walk a second and stand a second, one on the level floor and one over
    /// a patch rising a fifth of a metre per metre toward its front.
    ///
    /// **Standing**, every foot of the sloped one is on the slope: within
    /// 15 mm of the height the same foot stands over the level floor, which
    /// is its pad joint's own two centimetres off the sole, laid along the
    /// slope's normal rather than up. The level one's feet against the same
    /// slope are a span of 127 mm apart, the fore feet in the hill and the
    /// hind ones over it, so the bound is an eighth of the defect.
    ///
    /// **Walking**, no foot of the sloped one is ever in the hill, and on
    /// every frame the lowest is on it, where the level one's lowest is up to
    /// 88 mm into it. The second after the stop is judged only once settled:
    /// on four legs the idle's recovering step is solved at its rest height
    /// and lowered with the pelvis rather than seated on the ground under it
    /// (symbios-avatar 0.19.3, `Idle::drive_on`), so a fore foot stepping home
    /// uphill passes through the slope for a third of a second whatever
    /// ground this crate hands over.
    #[test]
    fn a_beast_on_a_sloped_patch_plants_its_feet_on_the_slope() {
        const GRADE: f32 = 0.2;
        let slope = |at: Vec3| at.z * GRADE;
        let mut app = app();
        let rig = symbios_avatar::Avatar::build_with(
            &AvatarRecord::new("Grounded", beast()),
            &symbios_avatar::AvatarConfig {
                atlas: 64,
                ..symbios_avatar::AvatarConfig::default()
            },
        )
        .expect("the beast builds")
        .rig;
        let reach = GroundPatch::reach(&rig).expect("a beast stands on its feet");
        let hill = Drive {
            ground: Some(GroundPatch::sampled(reach, 0.25, |at| at.y * GRADE)),
            ..Drive::default()
        };
        let [level, sloped] = spawn_small(&mut app, &beast(), 5, vec![Drive::default(), hill])[..]
        else {
            unreachable!()
        };
        let lowest = |soles: &[f32]| soles.iter().copied().fold(f32::MAX, f32::min);
        let mut deepest_on_the_level = 0.0f32;
        walk_then_stand(&mut app, &[level, sloped], 60, |app, frame| {
            let flat = soles(app, level, |_| 0.0);
            let hill = soles(app, sloped, slope);
            let off = soles(app, level, slope);
            if frame < 60 {
                deepest_on_the_level = deepest_on_the_level.min(lowest(&off));
                assert!(
                    lowest(&hill) > -0.005 && lowest(&hill) < 0.035,
                    "frame {frame}: walking up the slope, its lowest foot is {:.3} m off it: \
                     {hill:?}",
                    lowest(&hill)
                );
            } else if frame >= 90 {
                for (foot, (hill, flat)) in hill.iter().zip(&flat).enumerate() {
                    assert!(
                        (hill - flat).abs() < 0.015,
                        "frame {frame}: standing, foot {foot} is {hill:.3} m off the slope \
                         where it stands {flat:.3} m off the level floor"
                    );
                }
                let span = off.iter().copied().fold(f32::MIN, f32::max) - lowest(&off);
                assert!(
                    span > 0.1,
                    "the level floor's feet should be the slope's rise apart"
                );
            }
        });
        assert!(
            deepest_on_the_level < -0.05,
            "on the level floor some foot should have walked into the hill: {deepest_on_the_level}"
        );
    }

    #[test]
    fn a_body_moved_by_a_transform_works_out_its_own_speed() {
        // The remote-peer path: no velocity to read, only where the body has
        // got to.
        let mut drive = Drive::standing(Vec3::ZERO);
        drive.moved_to(Vec3::Z, 0.5);
        assert_eq!(drive.velocity, Vec3::Z * 2.0);
        assert_eq!(drive.at, Vec3::Z);

        drive.moved_to(Vec3::Z * 2.0, 0.0);
        assert_eq!(
            drive.velocity,
            Vec3::ZERO,
            "a frame with no time in it has no velocity, and an infinity reads as a launch"
        );
    }
}
