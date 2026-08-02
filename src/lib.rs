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

pub mod convert;
pub mod spawn;

pub use convert::{atlas_image, mesh_of, polymesh_to_bevy};
pub use spawn::{AvatarBody, AvatarJoints, AvatarPose, SpawnAvatar, spawn_avatar};

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
            (spawn::build_requested_avatars, spawn::apply_avatar_poses).chain(),
        );
    }
}
