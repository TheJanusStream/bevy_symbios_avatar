//! Drawing a [`symbios_avatar::Avatar`] in Bevy.
//!
//! The engine builds parametric 3D avatars from atproto records; this crate
//! draws them. Nothing here decides what a body looks like. Every shape,
//! weight, chart and colour comes from [`symbios_avatar::Avatar::build`], and
//! every number that decides how a body *moves* comes from
//! [`symbios_avatar::anim`] — this crate converts, uploads and applies.
//!
//! Where it has to make a choice the engine did not — how a material is
//! parameterised, which colour space a buffer is in — the choice is written
//! down beside it in [`convert`], because a difference between the two
//! renderers that is really a difference in those choices is exactly the false
//! diagnosis this crate is meant to prevent.
//!
//! # Why it exists
//!
//! This crate is a **second instrument**. The engine ships with a software
//! renderer, and every quality judgement made about a body so far has been made
//! through it. Twice that instrument has produced a false diagnosis on its own:
//! a blink that looked broken because lids and globes were drawn in one colour,
//! and UV seam normals that read as a skinning defect and sent a day's work
//! after the wrong culprit. A defect that appears in one renderer and not the
//! other is a defect in the renderer; one that appears in both is a defect in
//! the body. There is no way to tell those apart with one instrument.
//!
//! It is also the only place the shipping tier is real. The budget the engine is
//! judged against — one to three skinned draws, thirty thousand triangles, a
//! WebGL2 feature set — is a number in a document until something uploads the
//! meshes to a GPU and a frame comes back.
//!
//! # Drawing a body
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
//! A [`SpawnAvatar`] becomes a root entity, one entity per joint of the rig
//! hanging off it in the rig's own hierarchy, and one entity per drawn mesh —
//! which is also the shape the draw budget is stated in. Write an [`AvatarPose`]
//! on the root and the joints follow; [`AvatarJoints`] indexes the joint
//! entities, and [`AvatarBody`] keeps the whole built [`symbios_avatar::Avatar`]
//! for anything that wants to ask what it cost. [`spawn`] has the details.
//!
//! Bring your own camera and lights: this crate draws a body and holds no
//! opinion about the scene it stands in.
//!
//! # Moving a body
//!
//! [`AnimatorPlugin`] adds an [`Animator`] resource, ticks the engine's motion
//! every frame and writes the result onto components.
//!
//! ```no_run
//! use bevy::prelude::*;
//! use bevy_symbios_avatar::{Animator, AnimatorPlugin, AvatarPlugin, SpawnAvatar};
//! use symbios_avatar::{Archetype, AvatarRecord};
//!
//! App::new()
//!     .add_plugins((DefaultPlugins, AvatarPlugin, AnimatorPlugin))
//!     .add_systems(
//!         Startup,
//!         |mut commands: Commands, mut animator: ResMut<Animator>| {
//!             animator.walking = true;
//!             commands.spawn(SpawnAvatar::from(AvatarRecord::new(
//!                 "Someone",
//!                 Archetype::default(),
//!             )));
//!         },
//!     )
//!     .run();
//! ```
//!
//! The driving half needs no GUI and is always compiled; only the window that
//! steers it is behind the `editor` feature. [`animator`] has the rest.
//!
//! # Editing a record
//!
//! The `editor` module adds a control for every axis a
//! [`symbios_avatar::AvatarRecord`] can hold, so a body can be watched while an
//! axis moves rather than judged from four fixed angles after the fact. That
//! reads like a contradiction of the viewer's own rule — a body, a light and a
//! camera, nothing else, because every feature that is not a body is another
//! thing that could be blamed for what is on the screen — and it would be, but
//! for two things it owes that rule and pays.
//!
//! It edits the **record and only the record**, so nothing tuned through it is a
//! body no one else can rebuild. And it **never appears in a judgement image**:
//! the panel hides on a key and does not draw at all under the viewer's
//! `--shot`, so what is photographed is the same body, light and camera it
//! always was.
//!
//! # Features
//!
//! - **`editor`** (default) — the record editor's panel and the motion
//!   window, and with them the only dependencies this crate has beyond Bevy and
//!   the engine, `bevy_egui` and `serde_json`. `default-features = false`
//!   removes all three and leaves the drawing and the driving untouched.
//! - **`builtin-clips`** (off) — the engine's baked CC0 clip set, embedded in
//!   the binary, which is what [`Clips::builtin`] returns. Off by default
//!   because the artifact is 200 KiB and a consumer that only draws bodies
//!   should not pay for motion it never plays — least of all a wasm one. A
//!   consumer that fetches `clips.bin` at run time inserts its own [`Clips`]
//!   resource instead.
//!
//! Both windows draw in `bevy_egui`'s `EguiPrimaryContextPass`, so an app that
//! wants them adds `EguiPlugin` alongside these plugins. `examples/viewer.rs` is
//! the worked example of all of it.

// docs.rs builds with `--cfg docsrs` on nightly, which is what puts the
// "available on crate feature `editor`" badges on the gated items. Inert
// everywhere else.
#![cfg_attr(docsrs, feature(doc_auto_cfg))]

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
/// onto one is not a subtle failure: Bevy panics on applying the command.
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
