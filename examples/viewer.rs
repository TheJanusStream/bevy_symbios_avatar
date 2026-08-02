//! The second instrument.
//!
//! A body, a light, and a camera you can walk round it with. Nothing else, on
//! purpose: this is here so a body can be judged by eye through a real GPU, and
//! every feature that is not a body is another thing that could be blamed for
//! what is on the screen.
//!
//! ```text
//! cargo run --release --example viewer
//! cargo run --release --example viewer -- --seed 7
//! cargo run --release --example viewer -- --quadruped
//! cargo run --release --example viewer -- --shot body.png   # one frame, then quit
//! ```
//!
//! **Run it in release.** Building a body subdivides, binds, unwraps and paints
//! a megapixel atlas, and debug spends about half a minute on that.
//!
//! What to do with it:
//!
//! - Drag with the left mouse button to turn the body, scroll to move in.
//! - `W` walks. The gait comes from the engine, so a walk that reads wrong here
//!   and right in the software renderer is this crate's fault, and one that
//!   reads wrong in both is the engine's.
//! - `Space` re-rolls the seed and rebuilds, `P` saves a picture.
//! - `B` prints what the body costs, against the budget it is judged by.

use bevy::input::mouse::{AccumulatedMouseMotion, AccumulatedMouseScroll};
use bevy::prelude::*;
use bevy::render::view::screenshot::{Screenshot, save_to_disk};
use bevy_symbios_avatar::{AvatarBody, AvatarPlugin, AvatarPose, SpawnAvatar};
use symbios_avatar::anim::{gait, plant_feet_of};
use symbios_avatar::{
    Archetype, AvatarRecord, FootingConfig, Gait, Ground, Pose, QuadrupedParams, Stride,
};

/// How far the camera starts from the body, as a multiple of its height.
const START_BACK: f32 = 1.9;
/// Radians of turn per pixel of drag.
const TURN_PER_PIXEL: f32 = 0.006;
/// How fast the walk cycles, in cycles per second.
const CADENCE: f32 = 1.1;
/// How many frames to let a body appear in before photographing it.
const SETTLE: u32 = 12;
/// How many frames to wait for the picture before giving up on it.
const GIVE_UP: u32 = 600;

fn main() {
    App::new()
        .add_plugins((
            DefaultPlugins.set(WindowPlugin {
                primary_window: Some(Window {
                    title: "symbios avatar".into(),
                    ..default()
                }),
                ..default()
            }),
            AvatarPlugin,
        ))
        .init_resource::<Orbit>()
        .init_resource::<Walk>()
        .init_resource::<Shot>()
        .add_systems(Startup, (stage, body))
        .add_systems(
            Update,
            (orbit, walk, reroll, report, shoot)
                .after(bevy_symbios_avatar::spawn::apply_avatar_poses),
        )
        .run();
}

/// Where the camera is looking from.
#[derive(Resource)]
struct Orbit {
    turn: f32,
    pitch: f32,
    distance: f32,
    /// What it is looking at, set once a body exists to measure.
    centre: Vec3,
}

impl Default for Orbit {
    fn default() -> Self {
        Self {
            turn: 0.0,
            pitch: 0.12,
            distance: 3.0,
            centre: Vec3::Y,
        }
    }
}

/// How far through the walk cycle the body is, and whether it is walking.
#[derive(Resource, Default)]
struct Walk {
    cycle: f32,
    walking: bool,
}

/// Marks the avatar's root.
#[derive(Component)]
struct Subject;

/// A ground plane, a key light and a camera.
///
/// A figure in a void has nothing to be lit against and nothing to cast a
/// shadow on, and a cast shadow is the strongest single cue that a body
/// occupies space. That lesson came from the software renderer; it is the same
/// lesson here.
fn stage(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    commands.spawn((
        Mesh3d(meshes.add(Plane3d::default().mesh().size(12.0, 12.0))),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: Color::srgb(0.30, 0.31, 0.35),
            perceptual_roughness: 1.0,
            ..default()
        })),
    ));
    commands.spawn((
        DirectionalLight {
            illuminance: 9_000.0,
            shadows_enabled: true,
            ..default()
        },
        Transform::from_xyz(3.0, 6.0, 4.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
    commands.spawn((
        Camera3d::default(),
        Transform::default(),
        // A fill, because the other instrument has one. The software renderer
        // measures ambient occlusion and lights against it, so a viewer with a
        // single key light reads every downward-facing surface — the underside
        // of a jaw, a hand hanging at the hip — much darker than it does. That
        // is a difference in the lighting rig masquerading as a difference in
        // the body, which is the one thing a second instrument must not
        // manufacture.
        AmbientLight {
            color: Color::srgb(0.62, 0.66, 0.78),
            brightness: 900.0,
            affects_lightmapped_meshes: false,
        },
    ));
}

/// Asks for the body named on the command line.
fn body(mut commands: Commands) {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let archetype = if args.iter().any(|arg| arg == "--quadruped") {
        Archetype::Quadruped(QuadrupedParams::default())
    } else {
        Archetype::default()
    };
    let mut record = AvatarRecord::new("Viewed", archetype);
    if let Some(seed) = args
        .iter()
        .position(|arg| arg == "--seed")
        .and_then(|at| args.get(at + 1))
        .and_then(|seed| seed.parse::<i64>().ok())
    {
        record.reroll(seed);
    }
    commands.spawn((Subject, SpawnAvatar::from(record), Transform::default()));
}

/// Turns the camera round the body and frames it on what was actually built.
fn orbit(
    mut orbit: ResMut<Orbit>,
    motion: Res<AccumulatedMouseMotion>,
    scroll: Res<AccumulatedMouseScroll>,
    buttons: Res<ButtonInput<MouseButton>>,
    bodies: Query<&AvatarBody, Added<AvatarBody>>,
    mut camera: Query<&mut Transform, With<Camera3d>>,
) {
    // Framed on the body that exists rather than on a guess: a quadruped is
    // longer than it is tall, and a frame sized by height crops the ends off.
    for body in &bodies {
        let (lo, hi) = body.avatar.parts.body.bounds();
        orbit.centre = (lo + hi) * 0.5;
        orbit.distance = (hi - lo).max_element().max(0.2) * START_BACK;
    }

    if buttons.pressed(MouseButton::Left) {
        orbit.turn -= motion.delta.x * TURN_PER_PIXEL;
        orbit.pitch = (orbit.pitch + motion.delta.y * TURN_PER_PIXEL).clamp(-1.2, 1.2);
    }
    if scroll.delta.y != 0.0 {
        orbit.distance = (orbit.distance * (1.0 - scroll.delta.y * 0.1)).clamp(0.2, 40.0);
    }

    let Ok(mut transform) = camera.single_mut() else {
        return;
    };
    let from = orbit.centre
        + Vec3::new(
            orbit.turn.sin() * orbit.pitch.cos(),
            orbit.pitch.sin(),
            orbit.turn.cos() * orbit.pitch.cos(),
        ) * orbit.distance;
    *transform = Transform::from_translation(from).looking_at(orbit.centre, Vec3::Y);
}

/// Walks the body while `W` is held.
///
/// The gait, the stride and the footing all come from the engine. Nothing here
/// decides how a body moves — which is the point, since a walk that reads
/// differently in the two instruments is the thing worth finding.
fn walk(
    keys: Res<ButtonInput<KeyCode>>,
    time: Res<Time>,
    mut state: ResMut<Walk>,
    mut commands: Commands,
    bodies: Query<(Entity, &AvatarBody)>,
) {
    let walking = keys.pressed(KeyCode::KeyW);
    if !walking && !state.walking {
        return;
    }
    state.walking = walking;
    if walking {
        state.cycle = (state.cycle + time.delta_secs() * CADENCE).fract();
    }

    for (entity, body) in &bodies {
        let rig = &body.avatar.rig;
        let mut pose = Pose::rest(rig);
        if walking {
            let gait = Gait::natural(rig);
            let stride = Stride::for_body(rig, 1.0);
            let steps = gait::step(rig, &mut pose, &gait, &stride, state.cycle);
            gait::swing_arms(rig, &mut pose, &gait, state.cycle);
            plant_feet_of(
                rig,
                &mut pose,
                &steps.stance,
                |foot| Some(Ground::level(Vec3::new(foot.x, 0.0, foot.z))),
                &FootingConfig::default(),
            );
        }
        commands.entity(entity).insert(AvatarPose(pose));
    }
}

/// Rebuilds the body on a fresh seed.
fn reroll(
    keys: Res<ButtonInput<KeyCode>>,
    mut commands: Commands,
    subjects: Query<Entity, With<Subject>>,
) {
    if !keys.just_pressed(KeyCode::Space) {
        return;
    }
    for entity in &subjects {
        // Despawned and asked for again rather than patched: a re-roll changes
        // the skeleton, so every mesh, chart and weight is a different one.
        commands.entity(entity).despawn();
    }
    let mut record = AvatarRecord::new("Viewed", Archetype::default());
    // A seed from the frame count would be reproducible; one from the clock is
    // what a re-roll button is for.
    record.reroll(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            // The low half only, which is the part of a nanosecond clock that
            // differs between two presses. A seed is an arbitrary label, so
            // discarding the high bits costs nothing.
            .map_or(1, |since| {
                i64::from_ne_bytes(
                    u64::try_from(since.as_nanos() & u128::from(u64::MAX))
                        .unwrap_or_default()
                        .to_ne_bytes(),
                )
            }),
    );
    commands.spawn((Subject, SpawnAvatar::from(record), Transform::default()));
}

/// Whether a screenshot has been asked for, and how long to wait for it.
///
/// A body takes a few frames to appear: it is built in `Update`, and the frame
/// after that is the first one that has anything in it. Shooting immediately
/// photographs an empty room, which looks exactly like a body that failed to
/// build.
#[derive(Resource, Default)]
struct Shot {
    frames: u32,
    taken: bool,
}

/// Saves a picture and quits, for `--shot <path>`.
///
/// Not a convenience. A renderer that has only ever been compiled is a
/// renderer nobody has looked through, and this crate exists to be looked
/// through. `P` does the same thing without quitting.
fn shoot(
    mut commands: Commands,
    mut state: ResMut<Shot>,
    keys: Res<ButtonInput<KeyCode>>,
    mut exit: MessageWriter<AppExit>,
) {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let asked = args
        .iter()
        .position(|arg| arg == "--shot")
        .and_then(|at| args.get(at + 1));

    if keys.just_pressed(KeyCode::KeyP) {
        commands
            .spawn(Screenshot::primary_window())
            .observe(save_to_disk("viewer.png"));
        return;
    }

    let Some(path) = asked else { return };
    state.frames += 1;

    if state.taken {
        // Waiting for the file, not counting frames to it. A capture is read
        // back across frames and then written on a task pool thread, and the
        // first version of this quit three frames after asking and produced no
        // picture at all — which looks exactly like a renderer that drew
        // nothing.
        if std::path::Path::new(path).exists() || state.frames > GIVE_UP {
            exit.write(AppExit::Success);
        }
        return;
    }
    if state.frames < SETTLE {
        return;
    }

    state.taken = true;
    commands
        .spawn(Screenshot::primary_window())
        .observe(save_to_disk(path.clone()));
}

/// Prints what the body costs, against the budget it is judged by.
fn report(keys: Res<ButtonInput<KeyCode>>, bodies: Query<&AvatarBody>) {
    if !keys.just_pressed(KeyCode::KeyB) {
        return;
    }
    for body in &bodies {
        let budget = body.avatar.budget;
        info!(
            "{} tris / 30000, {} draws / 3, {} joints, {} KiB texture",
            budget.tris,
            budget.meshes,
            budget.joints,
            budget.texture_bytes / 1024
        );
    }
}
