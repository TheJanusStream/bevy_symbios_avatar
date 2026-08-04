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
//! quaternions. The one exception is a blink, which is geometry until the face
//! has a rig of its own — see [`crate::spawn::AvatarClosure`].

use bevy::prelude::*;
use symbios_avatar::anim::{GazeConfig, gait, gaze, plant_feet_of};
use symbios_avatar::{Blink, FootingConfig, Gait, Ground, Pose, Stride, Zone};

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
    /// Whether the gaze follows a target circling the body.
    pub tracking: bool,
    /// Radians per second that target travels.
    pub gaze_speed: f32,
    /// Where the target sits when it is not circling, in radians.
    pub gaze_angle: f32,
    /// Furthest the whole chain may turn from facing forward, in radians.
    pub gaze_limit: f32,
    /// The engine's blink timer.
    blink: Blink,
    /// How long the body has been alive, for circling the gaze target.
    elapsed: f32,
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
            tracking: true,
            // Slow enough that the head is plainly tracking rather than
            // snapping, which is the thing being judged.
            gaze_speed: 0.6,
            gaze_angle: 0.0,
            gaze_limit: GazeConfig::default().limit,
            blink: Blink::seeded(7),
            elapsed: 0.0,
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
        !self.walking && !self.blinking && !self.tracking
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
        app.init_resource::<Animator>().add_systems(
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
    mut animator: ResMut<Animator>,
    bodies: Query<(Entity, Ref<AvatarBody>)>,
) {
    let asked = animator.is_changed();
    if animator.is_idle() && !asked {
        return;
    }

    let delta = time.delta_secs();
    // Every write below is guarded on the thing it advances actually running,
    // which is what keeps the change-detection signal above from latching on.
    if animator.walking && !animator.scrub {
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

    for (entity, body) in &bodies {
        let rig = &body.avatar.rig;
        let mut pose = Pose::rest(rig);
        if animator.walking {
            let gait = animator.gait.of(rig);
            let stride = Stride::for_body(rig, animator.pace);
            let steps = gait::step(rig, &mut pose, &gait, &stride, animator.cycle);
            if animator.swing_arms {
                gait::swing_arms(rig, &mut pose, &gait, animator.cycle);
            }
            if animator.footing {
                plant_feet_of(
                    rig,
                    &mut pose,
                    &steps.stance,
                    |foot| Some(Ground::level(Vec3::new(foot.x, 0.0, foot.z))),
                    &FootingConfig::default(),
                );
            }
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
        commands.entity(entity).insert(AvatarPose(pose));
        // A closure is geometry, not a transform: writing one rebuilds two
        // meshes. That is the honest cost of a blink and nothing else should
        // pay it, so a held closure is written when it is asked for and when a
        // freshly built body has not been told about it yet — a rebuild spawns
        // with the lids open, and a body that silently reopened its eyes on
        // every re-roll reads exactly like a closure that does not stick.
        if animator.blinking || asked || body.is_added() {
            commands.entity(entity).insert(AvatarClosure(closure));
        }
    }
}

/// The window.
///
/// A window rather than a panel, and deliberately so: the record editor claims
/// an edge of the screen because it is long and is read top to bottom, and this
/// is short, consulted in passing, and belongs somewhere the body is not.
#[cfg(feature = "editor")]
pub fn animator_panel(mut contexts: bevy_egui::EguiContexts, mut animator: ResMut<Animator>) {
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

            ui.separator();
            ui.toggle_value(&mut animator.blinking, "blink");
            ui.add_enabled(
                !animator.blinking,
                egui::Slider::new(&mut animator.closure, 0.0..=1.0)
                    .text("closure")
                    .fixed_decimals(3),
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
