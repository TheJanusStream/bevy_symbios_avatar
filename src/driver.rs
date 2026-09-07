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
use symbios_avatar::anim::driver::{
    Driven, Driver, DriverConfig, Hold, Inputs, Showing, Source, WalkFlags, level_ground,
    velocity_of,
};
use symbios_avatar::{Expression, Gait, Heading};

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
/// hold; see [`drive_avatar_bodies`].
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
            walk: WalkFlags::default(),
            turn: 0.0,
            lids: None,
            face: Expression::NEUTRAL,
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
/// # When to drive a body yourself instead
///
/// This system gives every body a **level floor** and lays **no layer** over
/// the motion, because neither a ground closure nor an overlay can live in a
/// component: both are borrows of things a system holds. A consumer that needs
/// either — real terrain under the feet, an authored clip or a face over the
/// pose — writes its own system that builds [`Inputs`] with those closures and
/// calls [`Driver::drive`](symbios_avatar::anim::Driver::drive) directly. That
/// is a dozen lines and no duplicated state machine, which is the division this
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
        let inputs = Inputs {
            delta,
            velocity: drive.velocity,
            at: drive.at,
            facing: drive.facing,
            heading: drive.heading,
            swimming: drive.swimming,
            gesture: gesture.as_deref(),
            hold: drive.hold,
            showing: drive.showing,
            cycle: drive.cycle,
            cadence: drive.cadence,
            gait: drive.gait.as_ref(),
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
        };
        let Some(driven) = driver.drive(&body.avatar.rig, &inputs, level_ground) else {
            continue;
        };
        commands.entity(entity).insert((
            Drove::from(&driven),
            AvatarPose(driven.pose),
            AvatarClosure(driven.closure),
        ));
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
        assert!(
            app.world().get::<crate::Blending>(body).is_none(),
            "the animator posed a body that carries its own driver"
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
