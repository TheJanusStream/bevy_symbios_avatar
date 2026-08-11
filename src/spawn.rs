//! Putting a built body into the world.
//!
//! An avatar becomes a small entity tree: a root, one entity per joint of the
//! rig hanging off it in the rig's own hierarchy, and one entity per drawn mesh
//! carrying a [`SkinnedMesh`] that points at every joint. That is the shape
//! Bevy's skinning wants, and it is also the shape the budget is stated in —
//! one draw per merged mesh.
//!
//! Two things about it are worth stating, because both are easy to get subtly
//! wrong and neither fails loudly.
//!
//! **A joint's transform is local, and the engine's [`Pose`] is too.** A
//! joint's rest offset is its position minus its parent's, its rotation comes
//! straight from `Pose::rotations`, and Bevy composes the hierarchy. That is
//! exactly what [`Pose::forward`] does, so the two agree by construction rather
//! than by a conversion someone has to keep in step.
//!
//! **The mesh entity's own transform is ignored.** Bevy replaces a skinned
//! mesh's model matrix with the skin matrix, so moving an avatar means moving
//! the joints — which is what happens anyway, since the joints hang off the
//! root. Setting a transform on the mesh entity and wondering why nothing moved
//! is the trap here.

use bevy::mesh::skinning::{SkinnedMesh, SkinnedMeshInverseBindposes};
use bevy::prelude::*;
use symbios_avatar::{Avatar, AvatarConfig, AvatarRecord, MeshKind, Pose};

use crate::convert::{atlas_image, mesh_of};

/// A request to build and draw a body.
///
/// Spawn one of these and [`crate::AvatarPlugin`] replaces it with the body it
/// describes. Building is not cheap — it meshes, subdivides, binds, unwraps and
/// paints — so it happens once, in a system, rather than being asked for every
/// frame.
#[derive(Component, Clone, Debug)]
pub struct SpawnAvatar {
    /// The body to build.
    pub record: AvatarRecord,
    /// How to build it. The defaults are what the engine's own tools use.
    pub config: AvatarConfig,
    /// How shut the eyes are, `0` open and `1` closed.
    ///
    /// A blink is geometry rather than a pose — nothing rigs a lid yet — so it
    /// is fixed at spawn. Changing it means respawning, which is a fair
    /// reflection of what it costs.
    pub closure: f32,
}

impl From<AvatarRecord> for SpawnAvatar {
    fn from(record: AvatarRecord) -> Self {
        Self {
            record,
            config: AvatarConfig::default(),
            closure: 0.0,
        }
    }
}

/// A body that has been built and drawn, on the root of its entity tree.
///
/// The whole [`Avatar`] is kept, not just its geometry. This crate exists to be
/// compared against another renderer, and every comparison worth making — what
/// it costs, where its head is, how thick its arm came out — is a question for
/// the engine's own types rather than for a pile of Bevy handles.
/// Not `Debug`: an [`Avatar`] owns several megabytes of texture and the engine
/// withholds `Debug` on purpose.
#[derive(Component)]
pub struct AvatarBody {
    /// The built body.
    pub avatar: Avatar,
}

/// The joint entities of a body, in the rig's own order.
///
/// Indexable by joint, which is what every part of the engine that talks about
/// joints uses.
#[derive(Component, Debug, Default, Clone)]
pub struct AvatarJoints(pub Vec<Entity>);

/// The pose a body is holding.
///
/// Write a new one and the joints follow. Absent, the body stands in its rest
/// pose and nothing is written every frame.
#[derive(Component, Debug, Clone)]
pub struct AvatarPose(pub Pose);

/// How shut a body's eyes are, `0` open and `1` shut.
///
/// Write a new one and the eyes follow, which is what makes a blink something
/// this crate can show rather than only describe. A blink is geometry — a lid
/// swung about the eye's pivot with no joint to drive it, so following one meant
/// rebuilding two small meshes rather than writing a transform.
///
/// **It is a pose now** (symbios-avatar#118): the four lids have joints, their
/// shells are part of the skin's own draw, and `Eyes::blink` writes the four
/// rotations onto whatever pose the body is already holding. This component
/// survives as the RECORD of what the lids are holding — the animator writes it
/// so anything that wants to ask can — and drives no geometry at all.
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub struct AvatarClosure(pub f32);

/// Builds every body that has been asked for.
///
/// Runs in `Update` rather than at startup so a body can be asked for at any
/// time, which is what a viewer that re-rolls a seed needs.
pub fn build_requested_avatars(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut bindposes: ResMut<Assets<SkinnedMeshInverseBindposes>>,
    requests: Query<(Entity, &SpawnAvatar)>,
) {
    for (entity, request) in &requests {
        commands.entity(entity).remove::<SpawnAvatar>();
        let Some(avatar) = Avatar::build_with(&request.record, &request.config) else {
            // A record that describes no body is a record, not a crash. The
            // engine returns None for exactly one reason — limbs that overlap
            // at a joint — and a viewer should say so rather than fall over.
            warn!("a record described a body that could not be built");
            continue;
        };
        spawn_avatar(
            &mut commands,
            entity,
            avatar,
            request.closure,
            &mut meshes,
            &mut materials,
            &mut images,
            &mut bindposes,
        );
    }
}

/// Draws a built body under `root`.
///
/// Separate from the system so a caller that already has an [`Avatar`] — one
/// built off the main thread, or one being compared against another — does not
/// have to go back through a record to draw it.
#[expect(
    clippy::too_many_arguments,
    reason = "four asset stores and a body; splitting them would only hide them"
)]
pub fn spawn_avatar(
    commands: &mut Commands,
    root: Entity,
    avatar: Avatar,
    closure: f32,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
    bindposes: &mut Assets<SkinnedMeshInverseBindposes>,
) {
    // Everything below hangs off `root`, and Bevy propagates both transforms
    // and visibility down a hierarchy — so a root that carries neither leaves
    // every mesh and joint under it warning about an inconsistent parent, and
    // the body does not draw. `if_new`, because a caller who placed the body
    // somewhere meant it.
    commands
        .entity(root)
        .insert_if_new((Transform::default(), Visibility::default()));

    let joints = spawn_joints(commands, root, &avatar);

    // The rest pose is the bind pose: every joint unrotated at the position the
    // rig was built with, so undoing it is a translation and nothing more.
    let inverse = bindposes.add(SkinnedMeshInverseBindposes::from(
        avatar
            .rig
            .joints
            .iter()
            .map(|joint| Mat4::from_translation(-joint.position))
            .collect::<Vec<_>>(),
    ));

    let atlas = images.add(atlas_image(&avatar.skin));
    // The body's own meshes, then the eyes, rather than the one list
    // `Avatar::drawn` hands over. Kept as two lists because the globes are
    // built per call rather than merged, not because either half is going to be
    // handed back new geometry: since symbios-avatar#118 a blink is a pose.
    let eyes = avatar.eyes_at(closure);
    for drawn in avatar.meshes.iter().chain(&eyes) {
        let material = materials.add(material_for(drawn.kind, &atlas));
        let mesh = commands
            .spawn((
                Mesh3d(meshes.add(mesh_of(drawn))),
                MeshMaterial3d(material),
                // Ignored for a skinned mesh — see the module note — but a mesh
                // entity still needs one to have a place in the hierarchy.
                Transform::default(),
                SkinnedMesh {
                    inverse_bindposes: inverse.clone(),
                    joints: joints.clone(),
                },
                ChildOf(root),
            ))
            .id();
        let _ = mesh;
    }
    commands.entity(root).insert(AvatarClosure(closure));

    commands
        .entity(root)
        .insert((AvatarJoints(joints), AvatarBody { avatar }));
}

/// Spawns one entity per joint, in the rig's hierarchy, at the rest pose.
fn spawn_joints(commands: &mut Commands, root: Entity, avatar: &Avatar) -> Vec<Entity> {
    let mut entities: Vec<Entity> = Vec::with_capacity(avatar.rig.len());
    for joint in &avatar.rig.joints {
        // A joint's transform is its offset from its parent, which is what
        // makes Bevy's composition agree with Pose::forward.
        let parent = joint.parent.map_or(root, |parent| entities[parent]);
        let offset = joint.parent.map_or(joint.position, |at| {
            joint.position - avatar.rig.joints[at].position
        });
        entities.push(
            commands
                .spawn((Transform::from_translation(offset), ChildOf(parent)))
                .id(),
        );
    }
    entities
}

/// Writes a body's pose onto its joints.
///
/// Only when the pose changed. A rig is a few dozen entities and writing them
/// every frame would work; not writing them is how a viewer stays honest about
/// what a still body costs.
pub fn apply_avatar_poses(
    bodies: Query<(&AvatarPose, &AvatarJoints, &AvatarBody), Changed<AvatarPose>>,
    mut transforms: Query<&mut Transform>,
) {
    for (pose, joints, body) in &bodies {
        let rig = &body.avatar.rig;
        for (index, &entity) in joints.0.iter().enumerate() {
            let Ok(mut transform) = transforms.get_mut(entity) else {
                continue;
            };
            let joint = rig.joints[index];
            let rest = joint.parent.map_or(joint.position, |at| {
                joint.position - rig.joints[at].position
            });
            transform.translation = match joint.parent {
                Some(_) => rest,
                // The root carries the pose's own offset, exactly as
                // Pose::forward applies it.
                None => rest + pose.0.translation,
            };
            transform.rotation = pose.0.rotations.get(index).copied().unwrap_or_default();
        }
    }
}

/// How each kind of mesh is shaded.
///
/// Deliberately plain. The point of this crate is to see what the engine built,
/// and a material with opinions of its own is a second variable in every
/// comparison. Skin takes the painted atlas; everything else carries its colour
/// on its vertices, which is what lets a head of hair be one draw and still
/// have a shade per lock.
fn material_for(kind: MeshKind, atlas: &Handle<Image>) -> StandardMaterial {
    let (roughness, metallic) = match kind {
        MeshKind::Skin => (0.72, 0.0),
        MeshKind::Hair => (0.35, 0.0),
        MeshKind::Cloth => (0.92, 0.0),
        // A globe is the one wet thing on a body, and a matte eye is the
        // single fastest way to make a face look dead.
        MeshKind::Eye => (0.08, 0.0),
    };
    StandardMaterial {
        base_color: Color::WHITE,
        base_color_texture: matches!(kind, MeshKind::Skin).then(|| atlas.clone()),
        perceptual_roughness: roughness,
        metallic,
        ..default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use symbios_avatar::{Archetype, MeshKind};

    /// A headless app with just enough of Bevy to build a body.
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
        .add_systems(
            Update,
            (build_requested_avatars, apply_avatar_poses).chain(),
        );
        app
    }

    fn spawn(app: &mut App) -> Entity {
        let entity = app
            .world_mut()
            .spawn(SpawnAvatar::from(AvatarRecord::new(
                "Spawned",
                Archetype::default(),
            )))
            .id();
        app.update();
        entity
    }

    #[test]
    fn a_record_becomes_a_body_with_a_joint_per_joint() {
        let mut app = app();
        let root = spawn(&mut app);
        let world = app.world();
        let body = world.get::<AvatarBody>(root).expect("the body was built");
        let joints = world.get::<AvatarJoints>(root).expect("and its joints");
        assert_eq!(joints.0.len(), body.avatar.rig.len());
        assert!(
            world.get::<SpawnAvatar>(root).is_none(),
            "the request outlived the body it asked for, so it will build again"
        );
    }

    #[test]
    fn a_body_costs_one_draw_per_merged_mesh() {
        // The budget is stated in draws, and this is the only place that number
        // is real rather than asserted.
        let mut app = app();
        let root = spawn(&mut app);
        let drawn = app
            .world()
            .get::<AvatarBody>(root)
            .expect("built")
            .avatar
            .budget
            .meshes;
        let mut query = app.world_mut().query::<(&Mesh3d, &SkinnedMesh)>();
        assert_eq!(query.iter(app.world()).count(), drawn);
    }

    #[test]
    fn every_drawn_mesh_is_skinned_to_the_whole_rig() {
        // Bevy indexes the joint palette by the same numbers the engine wrote
        // into the vertices, so a mesh bound to a subset of the rig would draw
        // parts of a body attached to the wrong bones.
        let mut app = app();
        let root = spawn(&mut app);
        let joints = app
            .world()
            .get::<AvatarJoints>(root)
            .expect("joints")
            .0
            .len();
        let mut query = app.world_mut().query::<&SkinnedMesh>();
        let skins: Vec<usize> = query
            .iter(app.world())
            .map(|skin| skin.joints.len())
            .collect();
        assert!(!skins.is_empty(), "nothing was drawn");
        assert!(skins.iter().all(|count| *count == joints));
    }

    #[test]
    fn the_rest_pose_leaves_every_joint_where_the_rig_put_it() {
        // The one assertion that says the local-transform arithmetic is right.
        // Composed by Bevy, the joint entities have to land exactly where
        // Pose::forward puts them, or every bindpose is wrong by that error.
        let mut app = app();
        let root = spawn(&mut app);
        let rig = app
            .world()
            .get::<AvatarBody>(root)
            .expect("built")
            .avatar
            .rig
            .clone();
        let joints = app
            .world()
            .get::<AvatarJoints>(root)
            .expect("joints")
            .0
            .clone();

        let mut world_of = Vec::new();
        for &entity in &joints {
            let mut at = Vec3::ZERO;
            let mut walk = Some(entity);
            while let Some(current) = walk {
                if current == root {
                    break;
                }
                at += app
                    .world()
                    .get::<Transform>(current)
                    .expect("a transform")
                    .translation;
                walk = app.world().get::<ChildOf>(current).map(ChildOf::parent);
            }
            world_of.push(at);
        }

        let expected = Pose::rest(&rig).forward(&rig);
        for (index, at) in world_of.iter().enumerate() {
            assert!(
                at.distance(expected.positions[index]) < 1e-5,
                "joint {index} composed to {at:?}, not {:?}",
                expected.positions[index]
            );
        }
    }

    #[test]
    fn every_skin_mesh_takes_the_atlas_and_nothing_else_does() {
        // Everything that is not skin carries its colour on its vertices. A
        // garment sampling the skin atlas would be tinted by whatever part of a
        // body happens to sit at its UVs, which reads as a texturing bug in the
        // engine rather than in the drawing of it.
        //
        // Counted by kind rather than to a number: this test first asserted one
        // textured mesh and found two, because an avatar draws skin *twice* —
        // the body and the eyelids, which are skin and should be painted like
        // it. The engine was right and the expectation was wrong.
        let mut app = app();
        let root = spawn(&mut app);
        let expected = app
            .world()
            .get::<AvatarBody>(root)
            .expect("built")
            .avatar
            .drawn(0.0)
            .iter()
            .filter(|mesh| mesh.kind == MeshKind::Skin)
            .count();
        assert!(expected > 0, "a body drew no skin");

        let mut query = app
            .world_mut()
            .query::<(&MeshMaterial3d<StandardMaterial>,)>();
        let handles: Vec<_> = query.iter(app.world()).map(|(m,)| m.0.clone()).collect();
        let materials = app.world().resource::<Assets<StandardMaterial>>();
        let textured = handles
            .iter()
            .filter(|handle| {
                materials
                    .get(*handle)
                    .is_some_and(|material| material.base_color_texture.is_some())
            })
            .count();
        assert_eq!(
            textured, expected,
            "{textured} meshes sampled the skin atlas, against {expected} that are skin"
        );
    }

    #[test]
    fn shutting_the_eyes_turns_the_lid_joints_and_leaves_the_rest_of_the_rig_still() {
        // **This test used to read vertex positions**, because a blink was the
        // one thing a body did that a transform could not express and rebuilding
        // two meshes was the only way to see it. symbios-avatar#118 gave the
        // lids joints, so the contract this layer owes is the ordinary one: a
        // pose arrives, the joints it names turn, and nothing else does.
        //
        // Asserted on the joint entities rather than on the component, for the
        // reason the old version gave and which still holds: writing a pose and
        // having nothing happen is exactly the failure worth catching.
        let mut app = app();
        let root = spawn(&mut app);
        let body = app.world().get::<AvatarBody>(root).expect("built");
        let rig = body.avatar.rig.clone();
        let eyes = body
            .avatar
            .parts
            .eyes
            .as_ref()
            .expect("a biped has eyes")
            .clone();
        let lids: Vec<usize> = eyes.lids().map(|(_, joint)| joint).collect();
        assert_eq!(lids.len(), 4, "a pair of eyes has four lids");

        let mut pose = Pose::rest(&rig);
        eyes.blink(&mut pose, 1.0);
        app.world_mut().entity_mut(root).insert(AvatarPose(pose));
        app.update();

        let joints = app
            .world()
            .get::<AvatarJoints>(root)
            .expect("rigged")
            .0
            .clone();
        for (index, &entity) in joints.iter().enumerate() {
            let rotation = app
                .world()
                .get::<Transform>(entity)
                .expect("a joint entity")
                .rotation;
            let turned = rotation.angle_between(Quat::IDENTITY) > 1e-4;
            assert_eq!(
                turned,
                lids.contains(&index),
                "joint {index} turned: {turned}, and it {} a lid",
                if lids.contains(&index) {
                    "is"
                } else {
                    "is not"
                }
            );
        }
    }

    #[test]
    fn a_pose_moves_the_joints_it_names() {
        let mut app = app();
        let root = spawn(&mut app);
        let rig = app
            .world()
            .get::<AvatarBody>(root)
            .expect("built")
            .avatar
            .rig
            .clone();
        let shoulder = rig.in_zone(symbios_avatar::Zone::UpperLimb(
            symbios_avatar::Limb::ForeLeft,
        ))[0];
        let joints = app
            .world()
            .get::<AvatarJoints>(root)
            .expect("joints")
            .0
            .clone();

        let mut pose = Pose::rest(&rig);
        pose.rotations[shoulder] = Quat::from_rotation_z(0.5);
        app.world_mut().entity_mut(root).insert(AvatarPose(pose));
        app.update();

        let turned = app
            .world()
            .get::<Transform>(joints[shoulder])
            .expect("a transform")
            .rotation;
        assert!(
            turned.angle_between(Quat::IDENTITY) > 0.4,
            "the shoulder did not turn"
        );
    }

    #[test]
    fn a_hair_mesh_is_shaded_apart_from_a_skin_one() {
        // Merged geometry is grouped by material, so two kinds that shaded the
        // same would be one draw and the budget would be quietly wrong.
        let mut app = app();
        let root = spawn(&mut app);
        let kinds: Vec<MeshKind> = app
            .world()
            .get::<AvatarBody>(root)
            .expect("built")
            .avatar
            .drawn(0.0)
            .iter()
            .map(|mesh| mesh.kind)
            .collect();
        assert!(kinds.contains(&MeshKind::Hair) && kinds.contains(&MeshKind::Skin));
        let atlas = Handle::default();
        let apart = material_for(MeshKind::Hair, &atlas).perceptual_roughness
            - material_for(MeshKind::Skin, &atlas).perceptual_roughness;
        assert!(
            apart.abs() > 0.1,
            "hair and skin shade {apart} apart, which is not apart"
        );
    }
}
