//! Drawing a [`symbios_avatar::Avatar`] in Bevy.
//!
//! This crate is a **second instrument**, and that is the whole reason it
//! exists. The engine ships with a software renderer, and every quality
//! judgement made about a body so far has been made through it. Twice that
//! instrument has produced a false diagnosis on its own: a blink that looked
//! broken because lids and globes were drawn in one colour, and UV seam normals
//! that read as a skinning defect and sent a day's work after the wrong
//! culprit. A defect that appears in one renderer and not the other is a defect
//! in the renderer; one that appears in both is a defect in the body. There is
//! no way to tell those apart with one instrument.
//!
//! It is also the only place the shipping tier is real. The budget the engine is
//! judged against — one to three skinned draws, thirty thousand triangles, a
//! WebGL2 feature set — is a number in a document until something uploads the
//! meshes to a GPU and a frame comes back.
//!
//! ```no_run
//! use bevy::prelude::*;
//! use bevy_symbios_avatar::{AvatarPlugin, SpawnAvatar};
//! use symbios_avatar::{Archetype, AvatarRecord};
//!
//! App::new()
//!     .add_plugins((DefaultPlugins, AvatarPlugin))
//!     .add_systems(Startup, |mut commands: Commands| {
//!         commands.spawn(SpawnAvatar::from(AvatarRecord::new(
//!             "Someone",
//!             Archetype::default(),
//!         )));
//!     })
//!     .run();
//! ```
//!
//! Nothing here decides what a body looks like. Every shape, weight, chart and
//! colour comes from [`symbios_avatar::Avatar::build`]; this crate converts and
//! uploads. Where it has to make a choice the engine did not — how a material
//! is parameterised, which colour space a buffer is in — the choice is written
//! down beside it, because a difference between the two instruments that is
//! really a difference in those choices is exactly the false diagnosis this
//! crate is meant to prevent.
//!
//! # The panel, and the rule it does not break
//!
//! [`editor`] adds a control for every axis an
//! [`symbios_avatar::AvatarRecord`] can hold, so a body can be watched while an
//! axis moves rather than judged from four fixed angles after the fact. That
//! reads like a contradiction of the viewer's own rule — a body, a light and a
//! camera, nothing else, because every feature that is not a body is another
//! thing that could be blamed for what is on the screen — and it would be, but
//! for two things it owes that rule and pays.
//!
//! It edits the **record and only the record**, so nothing tuned through it is
//! a body no one else can rebuild. And it **never appears in a judgement
//! image**: the panel hides on a key and does not draw at all under `--shot`,
//! so what is photographed is the same body, light and camera it always was.
//! Turn the feature off with `default-features = false` and this crate is
//! exactly what it was.
//!
//! [`animator`] is the second window, and holds to the same line: every number
//! that decides how a body *moves* comes from [`symbios_avatar::anim`], and
//! this crate ticks a cycle and writes the result onto components. Its driving
//! half needs no GUI and is always compiled. The `builtin-clips` feature
//! embeds the engine's baked CC0 clip set so the window has clips to pick —
//! off by default, because the artifact is 200 KiB a consumer that never plays
//! it should not carry, and one that fetches `clips.bin` at run time inserts
//! its own [`Clips`] resource instead.

pub mod animator;
pub mod convert;
#[cfg(feature = "editor")]
pub mod editor;
pub mod spawn;

pub use animator::{
    Animator, AnimatorPlugin, Blending, Clips, GaitKind, floor_tilt, ground_normal,
};
pub use convert::{atlas_image, mesh_of, normal_image, orm_image, polymesh_to_bevy};
#[cfg(feature = "editor")]
pub use editor::{EditedAvatar, RecordEditor, RecordEditorPlugin};
pub use spawn::{AvatarBody, AvatarClosure, AvatarJoints, AvatarPose, SpawnAvatar, spawn_avatar};

use bevy::prelude::*;

/// The order a frame does things to a body in.
///
/// Three phases, and the ordering between them is a correctness requirement
/// rather than tidiness. A body can be **destroyed and rebuilt** mid-frame —
/// that is what a re-roll is, and what every step of a slider in the record
/// editor is — and anything that decided what to do with a body before that
/// happened is holding an entity that no longer exists. Queuing a component
/// onto one is not a subtle failure: Bevy panics on applying the command, and
/// it did, the first time these two ran unordered.
///
/// Chained in [`Update`], which puts a command flush between each pair, so a
/// system in a later set never sees a body an earlier set removed.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AvatarSystems {
    /// Bodies come into existence and go out of it.
    Build,
    /// What each body should be doing is decided.
    Animate,
    /// The decision is written onto the entities.
    Apply,
}

/// Draws avatars.
///
/// Adds the system that turns a [`SpawnAvatar`] request into geometry, and
/// declares the [`AvatarSystems`] order every other plugin in this crate hangs
/// off. Bring your own camera and lights: this crate draws a body and holds no
/// opinion about the scene it stands in.
#[derive(Debug, Default, Clone, Copy)]
pub struct AvatarPlugin;

impl Plugin for AvatarPlugin {
    fn build(&self, app: &mut App) {
        app.configure_sets(
            Update,
            (
                AvatarSystems::Build,
                AvatarSystems::Animate,
                AvatarSystems::Apply,
            )
                .chain(),
        )
        .add_systems(
            Update,
            spawn::build_requested_avatars.in_set(AvatarSystems::Build),
        )
        .add_systems(
            Update,
            spawn::apply_avatar_poses.in_set(AvatarSystems::Apply),
        );
    }
}
