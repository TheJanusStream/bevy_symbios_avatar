//! The second instrument.
//!
//! A body, a light, and a camera you can walk round it with — and two windows,
//! one for what a body *is* and one for what it is *doing*. The rule this
//! started with stands: every feature that is not a body is another thing that
//! could be blamed for what is on the screen, so a judgement image never
//! contains one. `H` hides both, and `--shot` never opens them. What they buy
//! is the thing four fixed angles cannot give — an axis you can watch move, and
//! a gait you can hold still at one point in its cycle.
//!
//! Every invocation needs `-F builtin-clips`: the example requires the feature
//! — its clip picker is not a picker with nothing to pick — and cargo does not
//! turn required features on by itself.
//!
//! ```text
//! cargo run --release -F builtin-clips --example viewer
//! cargo run --release -F builtin-clips --example viewer -- --seed 7
//! cargo run --release -F builtin-clips --example viewer -- --quadruped
//! cargo run --release -F builtin-clips --example viewer -- --shot body.png   # one frame, then quit
//! cargo run --release -F builtin-clips --example viewer -- --face --shot face.png  # framed on the head
//! cargo run --release -F builtin-clips --example viewer -- --walk --shot walking.png
//! cargo run --release -F builtin-clips --example viewer -- --gait wave --pace 1.4 --shot wave.png
//! cargo run --release --example viewer -- --gait running                 # a real flight phase
//! cargo run --release --example viewer -- --leap 0.4                    # a jump (#243)
//! cargo run --release --example viewer -- --fall 1.0                    # a drop, no wind-up
//! cargo run --release --example viewer -- --ledge 0.8 --phase 0.6       # jump off a ledge, held
//! cargo run --release -F builtin-clips --example viewer -- --walk --phase 0.35 --cadence 1.6
//! cargo run --release -F builtin-clips --example viewer -- --clip Sprint --phase 0.35 --yaw 0.9  # a RUN
//! cargo run --release -F builtin-clips --example viewer -- --mane 0     # bald, for judging a jaw
//! cargo run --release -F builtin-clips --example viewer -- --still      # no blink, no tracking
//! cargo run --release -F builtin-clips --example viewer -- --talk       # the jaw speaks
//! cargo run --release -F builtin-clips --example viewer -- --open 0.2   # hold the jaw open, radians
//! cargo run --release -F builtin-clips --example viewer -- --closure 1  # hold the lids shut
//! cargo run --release -F builtin-clips --example viewer -- --gaze 0.8   # hold the gaze, radians
//! cargo run --release -F builtin-clips --example viewer -- --clip Walk  # a baked CC0 clip instead
//! cargo run --release -F builtin-clips --example viewer -- --clip Greeting --layer --walk  # over the gait
//! cargo run --release -F builtin-clips --example viewer -- --grade 0.2   # a hill to walk up
//! cargo run --release -F builtin-clips --example viewer -- --camber 0.2  # a hill to stand across
//! cargo run --release -F builtin-clips --example viewer -- --bare       # no windows at all
//! ```
//!
//! **Run it in release.** Building a body subdivides, binds, unwraps and paints
//! a megapixel atlas, and debug spends about half a minute on that.
//!
//! What to do with it:
//!
//! - **Right-drag orbits, middle-drag pans, the wheel zooms** — the same
//!   controls as the sibling application this project's GUI work comes from, so
//!   the hands that use one do not have to relearn the other. Left is left
//!   alone on purpose: it belongs to the windows, and a camera that also
//!   answered to it would fight every slider on the screen.
//! - The panel on the left edits the record. Drag an axis and the body follows
//!   at a draft atlas, about fourteen frames a second; a quarter of a second
//!   after it stops the full-size build lands. `copy` puts the record on the
//!   clipboard as JSON and `load` reads one back, which is how a body somebody
//!   was fiddling with becomes a body anybody can rebuild.
//! - The **motion** window steers the walk, the blink and the gaze. All three
//!   come from the engine, so a walk that reads wrong here and right in the
//!   software renderer is this crate's fault, and one that reads wrong in both
//!   is the engine's. `scrub` holds the gait at one phase, which is the only
//!   way to look at a foot plant.
//! - **Everything the motion window steers about a gait has a flag now** (#15).
//!   It had `--walk` and nothing else, so a captured frame could show one of
//!   the patterns, at one cadence, at one pace — and the window that
//!   reaches the rest is the one `--shot` deliberately never opens. `--gait`,
//!   `--cadence` and `--pace` close that, and `--phase` already held both a
//!   gait and a clip at a chosen point in the cycle.
//! - **A run is `--gait running`** (#15). It was `--clip Jog` or `--clip
//!   Sprint` until symbios-avatar#186, because the procedural gait genuinely
//!   could not run: every `Gait` constructor floored a two-legged body's duty
//!   at a half, which is the definition of a walk, so this flag had nothing to
//!   point at. It does now, and epic #237 is removing the clips that were
//!   covering for it.
//!   `the_viewer_can_select_a_run_and_every_other_gait_is_still_a_walk` holds
//!   it: exactly one selectable gait leaves the ground.
//! - `H` hides both windows, and `--shot` never shows them.
//! - `F` frames the camera on the body again. Editing a record never moves the
//!   camera — a view that shifts while you are changing something else is a view
//!   you cannot judge from — so re-framing is a key rather than a side effect.
//! - `W` walks while it is held, as it always did.
//! - `Space` re-rolls the seed and rebuilds, honouring the panel's locks.
//! - `P` saves a picture, with the windows hidden for the frame it captures.
//! - `B` prints what the body costs, against the budget it is judged by.

use bevy::prelude::*;
use bevy::render::view::screenshot::{Screenshot, save_to_disk};
use bevy_egui::EguiPlugin;
use bevy_panorbit_camera::{PanOrbitCamera, PanOrbitCameraPlugin, PanOrbitCameraSystemSet};
use bevy_symbios_avatar::animator::{Animator, AnimatorPlugin, GaitKind, floor_tilt};
use bevy_symbios_avatar::editor::{RecordEditor, RecordEditorPlugin};
use bevy_symbios_avatar::{AvatarBody, AvatarPlugin, Clips};
use symbios_avatar::{Archetype, AvatarRecord, QuadrupedParams};

/// How far the camera starts from the body, as a multiple of its longest side.
const START_BACK: f32 = 1.9;

/// The same, for `--face`, as a multiple of the head's own radius.
///
/// Larger than it looks because the face camera is a 24-degree portrait lens
/// rather than the default 45: the head fills the frame at this distance and
/// the perspective is a portrait's rather than a fisheye's.
///
/// **A face is judged at conversational range and this instrument could only
/// stand across the room** (#13). `--shot` takes one frame at whatever the
/// automatic framing chose, which is the whole body — so every in-app
/// judgement of a FACE ever made through this viewer was made at about eight
/// pixels to the centimetre, and symbios-avatar#6's criterion 5 is entirely
/// about faces. Interactively the answer was always "scroll the wheel"; there
/// was no answer at all for a captured frame.
const FACE_BACK: f32 = 6.5;
/// Pitch the camera starts at, in radians.
const START_PITCH: f32 = 0.12;
/// How many frames to let a finished scene settle in before photographing it.
///
/// Counted from the frame the body is *complete*, not from startup. What is
/// left to wait for by then is the GPU catching up: the meshes, the atlas and
/// its two maps are handed over on one frame and uploaded across the next
/// few, and a material whose textures have not landed yet does not draw at
/// all — which is how a capture came back with a body but no hair, eyes or
/// cloth on it (#24).
const SETTLE: u32 = 12;
/// How many frames to wait on any one thing before giving up on it.
///
/// Both waits use it, each from its own start: the scene becoming complete,
/// and then the file appearing. A record that describes no body never
/// completes, and a viewer that hung forever on one would be worse than a
/// viewer that photographs the empty room and says so by the picture.
const GIVE_UP: u32 = 600;
/// How many frames to let a window disappear in before capturing.
const CLEAR: u32 = 2;

/// Whether a bare flag was passed on the command line.
fn flag(name: &str) -> bool {
    std::env::args().any(|arg| arg == name)
}

/// The word following a flag, if it was given one.
fn word(name: &str) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|arg| arg == name)
        .and_then(|at| args.get(at + 1))
        .cloned()
}

/// The number following a flag, if it was given one.
fn value(name: &str) -> Option<f32> {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|arg| arg == name)
        .and_then(|at| args.get(at + 1))
        .and_then(|value| value.parse().ok())
}

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
            AnimatorPlugin,
            EguiPlugin::default(),
            RecordEditorPlugin,
            PanOrbitCameraPlugin,
        ))
        .insert_resource(starting_editor())
        .insert_resource(starting_animator())
        .init_resource::<Shot>()
        .add_systems(Startup, (stage, pick_clip))
        .add_systems(Update, (frame_on_body, shortcuts, shoot, tilt_floor))
        .add_systems(
            PostUpdate,
            // Before the crate reads its input for the frame, which is what
            // makes the gate a gate rather than a correction applied late.
            gate_camera_on_gui.before(PanOrbitCameraSystemSet),
        )
        .run();
}

/// Marks the camera as one a body should be framed in.
#[derive(Component, Default)]
struct Framed {
    /// Whether a body has framed this camera already.
    done: bool,
    /// Whether somebody has asked for it to be framed again.
    asked: bool,
}

/// The ground the body stands on, so the slope control can tilt it.
#[derive(Component)]
struct Floor;

/// Tilts the floor onto the very plane the footing solve is being given.
///
/// **Both, or neither.** If the visible floor stayed flat the body would appear
/// to walk through or above it, and a viewer that lies about where the ground is
/// cannot be used to judge what a walk does on a slope — which is the whole
/// reason the control exists.
///
/// **Derived from the solve's own normal rather than re-expressed**, which is
/// the fix that outlives this particular pair of axes (#252). Written the other
/// way, as a rotation composed from the slope values, the two drifted apart
/// twice: #21 found the drawn tilt turning the opposite way to the solved one,
/// and #252 found it square to it after the solved surface moved from `+x` to
/// `+z` and this did not follow. Applying the tilt the library publishes
/// cannot be out of step with a surface built from the same definition,
/// whatever the axes become.
fn tilt_floor(animator: Res<Animator>, mut floor: Query<&mut Transform, With<Floor>>) {
    if !animator.is_changed() {
        return;
    }
    let tilt = floor_tilt(animator.grade, animator.camber);
    for mut transform in &mut floor {
        transform.rotation = tilt;
    }
}

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
        Floor,
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
        // **A portrait lens under `--face`, and it is not a nicety** (#13). A
        // face judged through the default 45-degree camera at three head radii
        // is a face photographed with a wide-angle lens from a foot away: the
        // nose is thrown forward, the ears fall away, and the judgement that
        // comes back is about the lens. Portraiture uses 85 mm on full frame
        // for exactly this reason, which is about 24 degrees vertical — so that
        // is what the face framing gets, and the distance in `FACE_BACK` is
        // set against it.
        Projection::from(PerspectiveProjection {
            fov: if flag("--face") {
                24.0f32.to_radians()
            } else {
                PerspectiveProjection::default().fov
            },
            ..default()
        }),
        Framed::default(),
        PanOrbitCamera {
            // The sibling application's bindings, and the reason for them is
            // not only habit. Left-drag is what a GUI uses, so a camera bound
            // to it has to be told to stand down over every window — and the
            // moment a drag crosses a window edge somebody has to decide who
            // owns it. Right and middle are never contested, so they can be
            // allowed through the gate unconditionally.
            button_orbit: MouseButton::Right,
            button_pan: MouseButton::Middle,
            // A stride is almost entirely forward and back, so it is nearly
            // invisible head-on: judged from a default camera this body's walk
            // reads far stiffer than the software renderer's sheet, which
            // includes a side view. That difference is the camera, not the
            // body.
            yaw: Some(value("--yaw").unwrap_or(0.0)),
            pitch: Some(START_PITCH),
            // A body is a metre or two across and the ground plane is twelve.
            // Without these the wheel zooms straight through a head, or out
            // until the subject is a speck.
            zoom_lower_limit: 0.2,
            zoom_upper_limit: Some(40.0),
            ..default()
        },
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

/// The record named on the command line, in an editor holding it.
///
/// The editor spawns the body itself, so the record the panel shows and the
/// body on screen are the same thing from the first frame. There is no second
/// place a record can come from and therefore no way for the two to disagree.
fn starting_editor() -> RecordEditor {
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
    // `--mane 0` for a bald judgement shot: hair is the loudest thing on a head
    // and the first thing in the way of judging a jaw, and the panel's controls
    // cannot be dragged by a script. Applied after the reroll so a seeded body
    // keeps everything else it rolled.
    //
    // **It silences all five regions now, not one length** (symbios-avatar
    // #209). It used to set the single `length` axis of a sculpted shell to
    // zero, which was the whole of the hair there was. A head grows hair in five
    // places now, and a flag that only shaved the scalp would leave a beard and
    // a pair of brows over exactly the jaw the flag exists to look at. Its job
    // is unchanged; what "all the hair" means is not.
    //
    // Both layers, because a painted region is hair too: a shaved chin still
    // has stubble drawn into its albedo, and stubble over a jawline is the same
    // obstruction at the same framing.
    if let Some(mane) = value("--mane") {
        if mane <= 0.0 {
            record.hair = symbios_avatar::HairRecord::bald();
        } else {
            // Anything above zero scales what the record already asked for,
            // rather than replacing it: a body keeps its own haircut and wears
            // less of it.
            for cut in [
                &mut record.hair.scalp.cut,
                &mut record.hair.brows.cut,
                &mut record.hair.moustache.cut,
                &mut record.hair.chin.cut,
                &mut record.hair.flanks.cut,
            ] {
                cut.length = (cut.length * mane).clamp(0.0, 1.0);
            }
        }
        record.sanitize();
    }
    let mut editor = RecordEditor::new(record);
    editor.open = windows_wanted();
    editor
}

/// What the body starts out doing, from the command line.
///
/// The flags survive the motion window rather than being replaced by it: a
/// still is captured by a command, and a command cannot click.
fn starting_animator() -> Animator {
    let live = !flag("--still");
    let mut animator = Animator::default();
    animator.open = windows_wanted();
    // Asking for a pattern is asking for it to be walked: a gait named and not
    // running is the rest pose, which is what `--gait standing` gives anyway.
    animator.walking = flag("--walk") || word("--gait").is_some();
    if let Some(name) = word("--gait") {
        let Some(kind) = GaitKind::named(&name) else {
            eprintln!("no gait called {name}. The patterns are:");
            for kind in GaitKind::ALL {
                eprintln!("  {}", kind.label());
            }
            eprintln!("a humanoid's run is a clip: --clip Jog, --clip Sprint");
            std::process::exit(1);
        };
        animator.gait = kind;
    }
    // Both from the same place the window's sliders write, and both left at the
    // resource's own default when they are not given: a flag that silently
    // re-tuned a gait would make every capture incomparable with every other.
    animator.cadence = value("--cadence").unwrap_or(animator.cadence);
    animator.pace = value("--pace").unwrap_or(animator.pace);
    // So a captured frame can be placed in the cycle rather than wherever the
    // twelfth frame happened to land. Judging a gait from one arbitrary phase
    // is how a walk gets called stiff when it has only ever been seen at
    // mid-stance.
    animator.cycle = value("--phase").unwrap_or(0.0);
    animator.scrub = value("--phase").is_some();
    // A blink is stochastic, so a single captured frame almost never catches
    // one. `--closure` holds the lids at a chosen point instead, which is what
    // makes the geometry path checkable from a still.
    animator.blinking = live && value("--closure").is_none();
    animator.closure = value("--closure").unwrap_or(0.0);
    // Talking is opt-in where blinking is opt-out: a body at rest blinks, but
    // a body that mutters to itself in every screenshot would be the viewer
    // manufacturing motion nobody asked to judge. `--open` holds the jaw at an
    // angle instead, exactly as `--closure` holds the lids.
    animator.talking = flag("--talk");
    animator.opening = value("--open").unwrap_or(0.0);
    // Same reason: a target that moves with the clock cannot be captured twice
    // at the same place, so a still cannot show what the gaze did without one.
    animator.tracking = live && value("--gaze").is_none();
    animator.gaze_angle = value("--gaze").unwrap_or(0.0);
    animator.grade = value("--grade").or_else(|| value("--slope")).unwrap_or(0.0);
    animator.camber = value("--camber").unwrap_or(0.0);
    // **A jump is the one motion that cannot be judged from a table** (engine
    // #243): the numbers say the wind-up, the flight and the landing MEET, and
    // a body can meet at every seam and still read as three animations played
    // in a row. `--phase` scrubs it, exactly as it scrubs a gait.
    animator.leap = value("--leap")
        .map(symbios_avatar::Leap::to_height)
        .or_else(|| value("--fall").map(symbios_avatar::Leap::falling))
        .or_else(|| {
            value("--ledge").map(|drop| {
                symbios_avatar::Leap::off_a_ledge(
                    symbios_avatar::Leap::to_height(0.3).launch(),
                    drop,
                )
            })
        });
    animator.layered = flag("--layer");
    animator
}

/// Points the animator at a clip named on the command line.
///
/// A startup system rather than part of [`starting_animator`], because the clip
/// set is a resource this crate inserts and the flags are parsed before any of
/// it exists. Naming a clip that is not there is a hard stop with the list
/// printed: a viewer that quietly ignored the flag would photograph the
/// procedural gait and label it the import, which is the one mistake a
/// comparison must not make.
fn pick_clip(clips: Res<Clips>, mut animator: ResMut<Animator>) {
    let Some(wanted) = word("--clip") else {
        return;
    };
    let Some(which) = clips.0.names().iter().position(|name| *name == wanted) else {
        eprintln!("no clip called {wanted}. This build carries:");
        for name in clips.0.names() {
            eprintln!("  {name}");
        }
        std::process::exit(1);
    };
    animator.clip = Some(which);
}

/// Whether the windows should be on screen at all.
///
/// A captured frame must be a body, a light and a camera and nothing else, so
/// under `--shot` they never open rather than being hidden at the moment of
/// capture. `--bare` is the same promise for a live session.
fn windows_wanted() -> bool {
    !flag("--bare") && !flag("--shot")
}

/// Frames the camera on the body, once, and again when `F` asks.
///
/// A quadruped is longer than it is tall, so the frame is sized on the body
/// that exists rather than on a guess about height.
///
/// **The camera is never moved by an edit**, and getting that wrong is what
/// this system was written twice for. A body is destroyed and rebuilt on every
/// step of a slider, so anything keyed on a new body being there is keyed on
/// the record being edited. The first version re-centred on every rebuild, on
/// the argument that a body should not walk out of frame as its height is taken
/// across its range — which sounds reasonable and is wrong in the hand: it
/// throws away the viewer's pan several times a second while an axis is being
/// dragged, and a view that moves while you are changing something else is a
/// view you cannot judge from. Nothing about a record's contents is the
/// camera's business.
///
/// So it frames the first body and then leaves the camera to whoever is holding
/// the mouse. `F` re-frames on demand, which is the whole of what the automatic
/// version was for and costs a keypress at the moment somebody actually wants
/// it.
fn frame_on_body(
    bodies: Query<&AvatarBody>,
    mut cameras: Query<(&mut PanOrbitCamera, &mut Framed)>,
) {
    let Some(body) = bodies.iter().next() else {
        return;
    };
    let (lo, hi) = body.avatar.parts.body.bounds();
    // The head's own joint, when `--face` asked for it: its position is the
    // focus and its radius sets the distance, so the framing follows a head
    // that `head_size` or a composite has resized rather than a constant
    // somebody would have to re-tune (#13).
    let face = flag("--face")
        .then(|| {
            let rig = &body.avatar.rig;
            rig.in_zone(symbios_avatar::Zone::Head)
                .first()
                .map(|&head| (rig.joints[head].position, rig.joints[head].radius))
        })
        .flatten();
    for (mut camera, mut framed) in &mut cameras {
        if framed.done && !framed.asked {
            continue;
        }
        // Both halves of each pair AND `force_update`, which is the crate's own
        // door for writing its state directly. Without it the camera never
        // moves at all: it decides there is work to do by comparing each value
        // against its target, so snapping the two together is indistinguishable
        // from having already arrived — the transform stays wherever
        // initialisation left it, and the first version of this fix put the
        // camera on the floor between the body's feet.
        camera.target_focus = match face {
            Some((at, _)) => at,
            None => (lo + hi) * 0.5,
        };
        camera.focus = camera.target_focus;
        camera.target_radius = match face {
            Some((_, radius)) => radius * FACE_BACK,
            None => (hi - lo).max_element().max(0.2) * START_BACK,
        };
        camera.radius = Some(camera.target_radius);
        camera.force_update = true;
        framed.done = true;
        framed.asked = false;
    }
}

/// Keeps the camera and the windows from fighting over the pointer.
///
/// The camera crate ships a `bevy_egui` feature that would do this, and it is
/// deliberately not enabled — the sibling application found its gate to be
/// all-or-nothing and privately scheduled, which left a right-drag dead the
/// moment it crossed a GUI window. This is that application's replacement, and
/// it is three rules:
///
/// - a **held right or middle button always drives the camera**, so an orbit or
///   a pan never dies because the drag started over a window or crossed one
/// - **scroll-zoom stays blocked while a window wants the pointer**, because
///   the wheel is how egui scrolls its own panels
/// - the gate looks at the **previous frame as well as this one**, because
///   `wants_pointer_input` flips true one frame late on a click into a window —
///   without that, the first frame of every click on a slider also orbits
fn gate_camera_on_gui(
    mut contexts: Query<&mut bevy_egui::EguiContext>,
    mouse: Res<ButtonInput<MouseButton>>,
    mut cameras: Query<&mut PanOrbitCamera>,
    mut wanted_last_frame: Local<bool>,
) {
    let mut wants = false;
    for mut context in &mut contexts {
        let context = context.get_mut();
        wants |= context.wants_pointer_input() || context.wants_keyboard_input();
    }
    let enable = mouse.any_pressed([MouseButton::Right, MouseButton::Middle])
        || (!wants && !*wanted_last_frame);
    *wanted_last_frame = wants;
    for mut camera in &mut cameras {
        // Written only when it changes: assigning every frame dirties the
        // component and defeats the crate's own change detection.
        if camera.enabled != enable {
            camera.enabled = enable;
        }
    }
}

/// Every key the viewer answers to, behind one guard.
///
/// One system rather than five, because the guard is the interesting part and
/// repeating it five times is five chances to forget it. Every shortcut is dead
/// while egui wants the keyboard: without that, typing a name into the record
/// panel walks the body on the `w`, hides both windows on the `h`, and re-rolls
/// the seed on the space bar.
///
/// - `F` frames the camera on the body again. The only thing that moves the
///   camera other than a hand on the mouse, and it takes a keypress precisely so
///   that editing a record never does.
/// - `W` walks while it is held. Kept alongside the motion window's own toggle
///   because a key is faster than reaching for one, and read on press and
///   release rather than as "walking = pressed" — the latter would overwrite
///   the window's toggle sixty times a second with a key nobody is touching.
/// - `Space` re-rolls to the NEXT seed, honouring the panel's locks. Not a seed
///   off the clock, which is a body nobody can go back to: the number that
///   produced it is discarded the moment the next press happens, so "the one
///   three re-rolls ago had the jaw I meant" is unanswerable. The panel has
///   somewhere to put the number now, and shows it.
/// - `H` hides and shows both windows together, because the promise is about
///   what a judgement image contains and half a GUI in a picture is still a GUI
///   in a picture.
/// - `B` prints what the body costs, against the budget it is judged by.
/// - `P` asks for a picture; [`shoot`] owns the rest, because a capture that
///   must not contain a window cannot happen on the frame the key was read.
fn shortcuts(
    mut contexts: bevy_egui::EguiContexts,
    keys: Res<ButtonInput<KeyCode>>,
    mut editor: ResMut<RecordEditor>,
    mut animator: ResMut<Animator>,
    mut shot: ResMut<Shot>,
    mut framed: Query<&mut Framed>,
    bodies: Query<&AvatarBody>,
) {
    if typing(&mut contexts) {
        return;
    }
    if keys.just_pressed(KeyCode::KeyF) {
        for mut framed in &mut framed {
            framed.asked = true;
        }
    }
    if keys.just_pressed(KeyCode::KeyW) {
        animator.walking = true;
    }
    if keys.just_released(KeyCode::KeyW) {
        animator.walking = false;
    }
    if keys.just_pressed(KeyCode::Space) {
        let next = editor.record.seed.wrapping_add(1);
        editor.reroll(next);
    }
    if keys.just_pressed(KeyCode::KeyH) {
        let showing = !editor.open;
        editor.open = showing;
        animator.open = showing;
    }
    if keys.just_pressed(KeyCode::KeyB) {
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
    if keys.just_pressed(KeyCode::KeyP) && shot.pending.is_none() && shot.reopen == 0 {
        shot.restore = editor.open;
        editor.open = false;
        animator.open = false;
        shot.pending = Some((String::from("viewer.png"), CLEAR));
    }
}

/// Whether a GUI window is taking the keyboard.
fn typing(contexts: &mut bevy_egui::EguiContexts) -> bool {
    contexts
        .ctx_mut()
        .is_ok_and(|context| context.wants_keyboard_input())
}

/// Whether a screenshot has been asked for, and how long to wait for it.
///
/// A body does not appear, it *arrives*: nothing at all, then a draft built at
/// a quarter atlas, then the real one, each landing off the compute pool
/// whenever it lands. None of that is a number of frames, so [`Shot`] does not
/// guess at one — it watches for the finished body and only then starts
/// counting.
#[derive(Resource, Default)]
struct Shot {
    /// Frames spent on the current wait — for the scene, then for the file.
    ///
    /// Restarted at the capture, because the two waits are separate budgets:
    /// a scene that took its time is not a reason to give up on the file.
    frames: u32,
    /// The frame the scene was first seen finished, once it has been.
    ready: Option<u32>,
    taken: bool,
    /// A picture `P` asked for, held until the windows are off the screen.
    ///
    /// Not a nicety. A judgement image with a UI in it is an image nobody can
    /// compare against the software renderer's sheet, and hiding a window in
    /// the same frame as the capture does not work — egui has already drawn by
    /// the time a key is read, so the picture would still have it in.
    pending: Option<(String, u32)>,
    /// Frames left before hidden windows go back up.
    reopen: u32,
    /// Whether the windows were open before a pending shot hid them.
    restore: bool,
}

/// Saves a picture and quits, for `--shot <path>`.
///
/// Not a convenience. A renderer that has only ever been compiled is a
/// renderer nobody has looked through, and this crate exists to be looked
/// through. `P` does the same thing without quitting.
fn shoot(
    mut commands: Commands,
    mut state: ResMut<Shot>,
    mut editor: ResMut<RecordEditor>,
    mut animator: ResMut<Animator>,
    bodies: Query<&AvatarBody>,
    mut exit: MessageWriter<AppExit>,
) {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let asked = args
        .iter()
        .position(|arg| arg == "--shot")
        .and_then(|at| args.get(at + 1));

    // Put back a frame *after* the capture rather than in the same one. Which
    // side of `Update` the egui pass runs is not this example's to know, and
    // guessing wrong puts a window back into the picture it was hidden for.
    if state.reopen > 0 {
        state.reopen -= 1;
        if state.reopen == 0 {
            editor.open = state.restore;
            animator.open = state.restore;
        }
        return;
    }
    if let Some((path, wait)) = state.pending.take() {
        if wait > 0 {
            state.pending = Some((path, wait - 1));
        } else {
            commands
                .spawn(Screenshot::primary_window())
                .observe(save_to_disk(path));
            state.reopen = CLEAR;
        }
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

    // **Wait for the scene, not for a number** (#24). The old version counted
    // twelve frames from startup and photographed whatever was there, which on
    // one run was a body still missing its hair, eyes and cloth. Two conditions
    // and both are needed: the editor says the finished body has landed — not
    // an outstanding edit, not a build still on the pool, not the draft atlas —
    // and a body is actually in the world, because the editor sets that flag on
    // the frame it *asks* for the spawn and the entity arrives a frame later.
    if state.ready.is_none() && editor.settled() && !bodies.is_empty() {
        state.ready = Some(state.frames);
    }
    let waited = match state.ready {
        Some(at) => state.frames - at >= SETTLE,
        // Never settles for a record that describes no body. Photograph the
        // room anyway rather than hang: an empty picture is a report, and the
        // panel has already said in words what went wrong.
        None => state.frames > GIVE_UP,
    };
    if !waited {
        return;
    }

    state.taken = true;
    // A fresh budget for the second wait — see [`Shot::frames`].
    state.frames = 0;
    commands
        .spawn(Screenshot::primary_window())
        .observe(save_to_disk(path.clone()));
}
