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

pub mod convert;
#[cfg(feature = "editor")]
pub mod editor;
pub mod spawn;

pub use convert::{atlas_image, mesh_of, polymesh_to_bevy};
#[cfg(feature = "editor")]
pub use editor::{EditedAvatar, RecordEditor, RecordEditorPlugin};
pub use spawn::{
    AvatarBody, AvatarClosure, AvatarEye, AvatarJoints, AvatarPose, SpawnAvatar, spawn_avatar,
};

use bevy::prelude::*;

/// Draws avatars.
///
/// Adds the system that turns a [`SpawnAvatar`] request into geometry. Bring
/// your own camera and lights: this crate draws a body and holds no opinion
/// about the scene it stands in.
#[derive(Debug, Default, Clone, Copy)]
pub struct AvatarPlugin;

impl Plugin for AvatarPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            (
                spawn::build_requested_avatars,
                spawn::apply_avatar_poses,
                spawn::apply_avatar_closures,
            )
                .chain(),
        );
    }
}
