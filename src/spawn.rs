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
//!
//! **A body's hair can have two tiers, and only one draws at a time** (#48).
//! Asked for with [`AvatarConfig::far_hair`], the engine builds a far tier beside
//! the near hair - the scalp as one smooth low-poly solid, the facial cards as
//! they are - and hands it back outside [`Avatar::meshes`]. It becomes one more
//! entity here, with the near hair's own material and skin, and the two carry a
//! [`VisibilityRange`] each so the camera's distance picks which one draws: see
//! [`HairLod`]. A body built without a far tier draws its hair at every distance,
//! exactly as before.

use bevy::asset::uuid_handle;
use bevy::camera::visibility::VisibilityRange;
use bevy::mesh::skinning::{SkinnedMesh, SkinnedMeshInverseBindposes};
use bevy::prelude::*;
use symbios_avatar::{Avatar, AvatarConfig, AvatarRecord, MeshKind, Pose};

use crate::convert::{atlas_image, mesh_of, normal_image, orm_image, strand_mask_image};

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
    /// How to build it.
    ///
    /// [`From<AvatarRecord>`] asks for the engine's defaults plus the far hair
    /// tier ([`AvatarConfig::far_hair`]), which [`spawn_avatar`] draws beyond
    /// [`HairLod::switch`]. A config built by hand gets a far tier only if it
    /// asks for one.
    pub config: AvatarConfig,
    /// How shut the eyes start, `0` open and `1` closed.
    ///
    /// Recorded onto the root as [`AvatarClosure`]. It does not move geometry:
    /// a blink is a pose — the four lids are joints — so
    /// shutting the eyes means writing an [`AvatarPose`] — which is what
    /// [`crate::AnimatorPlugin`] does with its own closure control.
    pub closure: f32,
}

impl From<AvatarRecord> for SpawnAvatar {
    fn from(record: AvatarRecord) -> Self {
        Self {
            record,
            config: AvatarConfig {
                far_hair: true,
                ..AvatarConfig::default()
            },
            closure: 0.0,
        }
    }
}

/// Which of a body's two hair tiers an entity draws (#48).
///
/// On both hair entities of a body that was built with a far tier, and on
/// nothing else: a body without one has a single hair entity, no tier and no
/// [`VisibilityRange`], and draws its hair at every distance.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HairTier {
    /// The engine's own hair, up to [`HairLod::switch`].
    Near,
    /// [`Avatar::far_hair`], from [`HairLod::switch`] out.
    Far,
}

/// Where every body's hair changes tier (#48).
///
/// **The distance is the camera's from the body's root**, not from its head:
/// Bevy measures a [`VisibilityRange`] from the entity's origin, and a skinned
/// mesh's entity sits at the root. Both tiers measure from the same point, which
/// is what keeps them from drawing together or leaving a gap.
///
/// **One value for the app, read at spawn and kept current by
/// [`crate::AvatarPlugin`].** Change the resource and every tiered body in the
/// world takes the new ranges on the next frame; a body spawned without the
/// plugin keeps the defaults it was spawned with. Bevy stores one entry per
/// distinct range and every body's tiers share the same two, so a crowd costs
/// two entries - which matters on WebGL2, where the table is a fixed uniform of
/// 64.
#[derive(Resource, Debug, Clone, Copy, PartialEq)]
pub struct HairLod {
    /// The camera distance, in metres from a body's root, at which the far
    /// tier takes over.
    ///
    /// 12 m by default, where the engine judged its far tier (#350) - on a
    /// 1080-line, 60-degree camera, about 78 pixels a metre there. A lens with
    /// more pixels a metre shows the far tier larger at the same distance, and
    /// wants a longer switch to show it at the size it was judged at.
    pub switch: f32,
    /// How wide a band, centred on [`Self::switch`], the tiers crossfade over.
    ///
    /// `0` switches in one frame. Anything wider dithers one tier out as the
    /// other dithers in, and draws BOTH for as long as the camera is inside the
    /// band: one draw more a body, and a checker of skin wherever the near
    /// hair covers something the far tier does not.
    ///
    /// **Keep it 0 on WebGL2.** In Bevy 0.19.1 the dither shader a non-zero
    /// margin switches on reads the range table as a 64-entry uniform there,
    /// while the bind group layout declares room for one entry, so the mesh
    /// pipeline fails validation and the app quits on the first frame a band
    /// is drawn - measured in Chromium with this crate (#48). A zero margin
    /// is abrupt and never compiles that shader.
    pub margin: f32,
}

impl Default for HairLod {
    fn default() -> Self {
        Self {
            switch: HAIR_SWITCH,
            margin: HAIR_MARGIN,
        }
    }
}

/// [`HairLod::switch`]'s default, in metres.
pub const HAIR_SWITCH: f32 = 12.0;

/// [`HairLod::margin`]'s default, in metres.
pub const HAIR_MARGIN: f32 = 0.0;

impl HairLod {
    /// The range a tier draws over.
    ///
    /// A zero margin gives Bevy's abrupt ranges, which need no dither in the
    /// shader. A band does, and then the near tier's own start is not `0..0`:
    /// the shader divides by the width of the margin the camera is in, and a
    /// zero width there is an infinity cast to an integer, which a GPU is free
    /// to get wrong.
    #[must_use]
    pub fn range(&self, tier: HairTier) -> VisibilityRange {
        let switch = self.switch.max(0.0);
        let half = (self.margin.max(0.0) / 2.0).min(switch);
        let band = (switch - half)..(switch + half);
        let abrupt = half == 0.0;
        match tier {
            HairTier::Near => VisibilityRange {
                start_margin: if abrupt { 0.0..0.0 } else { -1.0..0.0 },
                end_margin: band,
                use_aabb: false,
            },
            HairTier::Far => VisibilityRange {
                start_margin: band,
                end_margin: f32::MAX..f32::MAX,
                use_aabb: false,
            },
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
/// The record of what the lids are holding, and nothing more: the animator
/// writes it beside every pose it applies, so anything that wants to ask can.
/// It drives no geometry. A blink is a pose — the four
/// lids have joints, their shells are part of the skin's own draw, and
/// `Eyes::blink` writes the four rotations onto whatever pose the body is
/// already holding — so shutting the eyes means writing an [`AvatarPose`],
/// not this.
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

    let atlas = SkinMaps {
        albedo: images.add(atlas_image(&avatar.skin)),
        normal: images.add(normal_image(&avatar.skin)),
        orm: images.add(orm_image(&avatar.skin)),
    };
    // Uploaded by the first body, and every body after it samples that one.
    if !images.contains(&STRAND_MASK) {
        let mask = strand_mask_image(avatar.strand_mask());
        if let Err(error) = images.insert(&STRAND_MASK, mask) {
            warn!("the strand mask did not upload, so hair draws as whole cards: {error}");
        }
    }
    // The body's own meshes, then the eyes, rather than the one list
    // `Avatar::drawn` hands over. Kept as two lists because the globes are
    // built per call rather than merged, not because either half is going to be
    // handed back new geometry: since symbios-avatar#118 a blink is a pose.
    let eyes = avatar.eyes_at(closure);
    let skin = SkinnedMesh {
        inverse_bindposes: inverse,
        joints: joints.clone(),
    };
    let lod = HairLod::default();
    let mut hair = None;
    for drawn in avatar.meshes.iter().chain(&eyes) {
        let material = materials.add(material_for(drawn.kind, &atlas));
        let mesh = commands
            .spawn((
                Mesh3d(meshes.add(mesh_of(drawn))),
                MeshMaterial3d(material.clone()),
                // Ignored for a skinned mesh — see the module note — but a mesh
                // entity still needs one to have a place in the hierarchy.
                Transform::default(),
                skin.clone(),
                ChildOf(root),
            ))
            .id();
        if drawn.kind == MeshKind::Hair {
            hair = Some((mesh, material));
        }
    }
    // The far tier (#48): one entity more, drawn with the near hair's own
    // material - the far solid takes the strand mask's solid row, so the cut
    // keeps it whole - and the same skin, so it poses with the body. Spawned
    // after every other mesh, so the body's own meshes keep their order.
    if let (Some(far), Some((near, material))) = (&avatar.far_hair, hair) {
        commands
            .entity(near)
            .insert((HairTier::Near, lod.range(HairTier::Near)));
        commands.spawn((
            Mesh3d(meshes.add(mesh_of(far))),
            MeshMaterial3d(material),
            Transform::default(),
            skin,
            HairTier::Far,
            lod.range(HairTier::Far),
            ChildOf(root),
        ));
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

/// Keeps every tiered hair entity on the app's [`HairLod`].
///
/// Only writes a range that differs: every write marks the component changed,
/// and one changed range makes Bevy rebuild its whole table of them that frame.
pub fn retune_hair_tiers(
    lod: Res<HairLod>,
    mut tiers: Query<(Ref<HairTier>, &mut VisibilityRange)>,
) {
    let fresh = lod.is_changed();
    for (tier, mut range) in &mut tiers {
        if !fresh && !tier.is_added() {
            continue;
        }
        let wanted = lod.range(*tier);
        if *range != wanted {
            *range = wanted;
        }
    }
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

/// The engine's strand mask, as the one image every hair material samples
/// (#47).
///
/// **A fixed handle, filled by the first body to spawn.** The engine paints one
/// mask for every avatar a process builds, and uploading it per body would put
/// the same quarter of a megabyte on the GPU again for every body in a scene.
const STRAND_MASK: Handle<Image> = uuid_handle!("5d0a3c1e-8f47-4b2a-9e6d-7c3f1a9b2e84");

/// The alpha below which a hair card is not drawn.
///
/// Half, which is where the engine's own renderer cuts its cards, so the two
/// instruments draw the same locks.
const STRAND_CUT: f32 = 0.5;

/// The three textures one painted skin uploads as.
struct SkinMaps {
    albedo: Handle<Image>,
    normal: Handle<Image>,
    orm: Handle<Image>,
}

/// How each kind of mesh is shaded.
///
/// Deliberately plain. The point of this crate is to see what the engine built,
/// and a material with opinions of its own is a second variable in every
/// comparison. Skin takes the painted atlas; everything else carries its colour
/// on its vertices, which is what lets a head of hair be one draw and still
/// have a shade per lock.
///
/// **Hair is the one kind drawn from both sides, and that is the engine's
/// contract rather than an opinion here** (#46). The engine builds a lock as a
/// single-sided card turned to face outward, and its hair loft says in as many
/// words that a consumer draws cards with a double-sided material - its own
/// software renderer is two-sided by construction. Culled, a card seen from
/// behind or edge-on simply was not there: the inside of a hanging curtain,
/// the far side of the locks beside a face, the back of a beard's rim.
///
/// **And nothing asks for anisotropy**, though hair carries tangents and a
/// highlight along the strands is what anisotropy is for. Bevy 0.19 builds the
/// anisotropy frame only under its `pbr_anisotropy_texture` feature. Without
/// it, a non-zero strength still switches the anisotropic lighting on, through
/// a zero tangent, and a head of hair renders blown-out white; with it, every
/// `StandardMaterial` in the consuming app binds one more texture, which is one
/// past WebGL2's sixteen for an app whose own materials already sit at the
/// ceiling.
///
/// **Hair is also the one kind cut out of an image, and the image is the
/// engine's** (#47). A card is a rectangle, and a row of card ends is a picket
/// fence. The engine paints a strand mask - white, so the vertex colour still
/// carries every tone, with each lock's silhouette in its alpha - and lanes
/// every card's UVs into it. Masked rather than blended: a mask needs no
/// sorting, and Bevy's shadow pass honours it, so a card casts its lock's
/// shadow and not its rectangle's.
fn material_for(kind: MeshKind, atlas: &SkinMaps) -> StandardMaterial {
    // Bevy's own values for everything decided here for one kind and not for
    // the others, and for everything not decided here at all.
    let plain = StandardMaterial::default();
    let (roughness, reflectance, metallic) = match kind {
        // 1.0, not a taste: Bevy MULTIPLIES the factor into the roughness
        // texture, so anything less darkens every texel of the finish the
        // engine painted. The per-texel values live in the ORM map (#22).
        MeshKind::Skin => (1.0, plain.reflectance, 0.0),
        // **Roughness is the software renderer's own hair finish** (#46).
        // 0.35 had no provenance and put a plastic stripe on every flat card.
        // On a sheet of nine heads each turned through five views, 0.35 read
        // as plastic and 0.7 as matte grey on black hair; with the reflectance
        // below, 0.50, 0.55 and 0.60 all read as hair. 0.52 is the engine
        // renderer's number inside that range, so the two instruments agree
        // by construction.
        //
        // **Reflectance is the half the double-sided material made
        // necessary.** Bevy's fill light reflects most at a grazing angle, and
        // a card seen edge-on is nothing but a grazing angle: at the default
        // 0.5, every card drawn from behind wore a blue-grey veil, which
        // turning the fill off removed and no roughness up to 0.7 did. Below
        // an F0 of 2% Bevy extinguishes that reflection (the pre-baked
        // specular occlusion in its ambient light); 0.25 is an F0 of 1%, which
        // halves it and quarters the highlight, and is where the veil left the
        // sheet.
        MeshKind::Hair => (0.52, 0.25, 0.0),
        MeshKind::Cloth => (0.92, plain.reflectance, 0.0),
        // A globe is the one wet thing on a body, and a matte eye is the
        // single fastest way to make a face look dead.
        MeshKind::Eye => (0.08, plain.reflectance, 0.0),
    };
    let skin = matches!(kind, MeshKind::Skin);
    let cards = matches!(kind, MeshKind::Hair);
    StandardMaterial {
        base_color: Color::WHITE,
        base_color_texture: match kind {
            MeshKind::Skin => Some(atlas.albedo.clone()),
            MeshKind::Hair => Some(STRAND_MASK),
            MeshKind::Cloth | MeshKind::Eye => None,
        },
        alpha_mode: if cards {
            AlphaMode::Mask(STRAND_CUT)
        } else {
            plain.alpha_mode
        },
        normal_map_texture: skin.then(|| atlas.normal.clone()),
        // One image, two slots: G/B feed metallic-roughness, R feeds
        // occlusion — which is exactly the ORM layout the engine bakes.
        metallic_roughness_texture: skin.then(|| atlas.orm.clone()),
        occlusion_texture: skin.then(|| atlas.orm.clone()),
        perceptual_roughness: roughness,
        reflectance,
        metallic,
        // Both halves of drawing a card from behind: `double_sided` turns a
        // back face's normal round to the viewer, and `cull_mode` is what stops
        // the rasteriser discarding the face before that can matter.
        double_sided: cards,
        cull_mode: if cards { None } else { plain.cull_mode },
        ..plain
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
        .init_resource::<HairLod>()
        .add_systems(
            Update,
            (
                build_requested_avatars,
                apply_avatar_poses,
                retune_hair_tiers,
            )
                .chain(),
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

    /// How many of the app's meshes draw with the camera `distance` metres from
    /// every body's root: Bevy's own rule, a mesh with no range always and one
    /// with a range when [`VisibilityRange::is_visible_at_all`] says so.
    fn drawn_at(app: &mut App, distance: f32) -> (usize, usize) {
        let mut query = app
            .world_mut()
            .query::<(&SkinnedMesh, Option<&VisibilityRange>, Option<&HairTier>)>();
        let mut drawn = 0;
        let mut hair = 0;
        for (_, range, tier) in query.iter(app.world()) {
            if range.is_none_or(|range| range.is_visible_at_all(distance)) {
                drawn += 1;
                hair += usize::from(tier.is_some());
            }
        }
        (drawn, hair)
    }

    #[test]
    fn a_body_costs_one_draw_per_merged_mesh_at_any_distance() {
        // The budget is stated in draws, and this is the only place that number
        // is real rather than asserted. **Since #48 a body has one mesh entity
        // more than it draws**: the far hair tier, which stands in for the near
        // hair past `HairLod::switch` and never beside it (the owner's margin
        // is 0). #48's own text asked for "one extra draw and no more"; the
        // engine put the far tier outside `meshes` (#350) precisely so there is
        // no extra draw, and this holds it to that at distances either side of
        // the switch and a hair's breadth from it.
        let mut app = app();
        let root = spawn(&mut app);
        let budget = app
            .world()
            .get::<AvatarBody>(root)
            .expect("built")
            .avatar
            .budget
            .meshes;
        let mut query = app.world_mut().query::<(&Mesh3d, &SkinnedMesh)>();
        assert_eq!(
            query.iter(app.world()).count(),
            budget + 1,
            "a body is its meshes and one far tier"
        );
        for distance in [0.0, 1.5, 11.99, 12.0, 12.01, 40.0, 1.0e6] {
            let (drawn, hair) = drawn_at(&mut app, distance);
            assert_eq!(
                (drawn, hair),
                (budget, 1),
                "at {distance} m {drawn} meshes drew, {hair} of them hair"
            );
        }
    }

    #[test]
    fn a_crossfade_band_is_the_only_place_both_tiers_draw() {
        // The liveness of the test above: the same reading can see two hair
        // tiers draw, and does, only inside a margin somebody asked for. Set
        // through the resource, so it also holds the plugin's retune to its
        // word for a body that is already standing there.
        let mut app = app();
        let root = spawn(&mut app);
        let budget = app
            .world()
            .get::<AvatarBody>(root)
            .expect("built")
            .avatar
            .budget
            .meshes;
        app.insert_resource(HairLod {
            switch: 20.0,
            margin: 2.0,
        });
        app.update();
        assert_eq!(drawn_at(&mut app, 18.5), (budget, 1));
        assert_eq!(drawn_at(&mut app, 19.5), (budget + 1, 2), "inside the band");
        assert_eq!(drawn_at(&mut app, 20.5), (budget + 1, 2), "inside the band");
        assert_eq!(drawn_at(&mut app, 21.5), (budget, 1));
    }

    #[test]
    fn the_far_tier_is_the_engines_far_hair_on_the_near_hairs_material_and_skin() {
        // What #350 asked of a consumer: the same material handle - the far
        // solid takes the strand mask's solid row, so the one hair material
        // draws both - and the same joints and bindposes, so the far tier poses
        // with the body it stands in for. A far tier on its own skin would
        // stand still while the head turned.
        let mut app = app();
        let root = spawn(&mut app);
        let far_vertices = app
            .world()
            .get::<AvatarBody>(root)
            .expect("built")
            .avatar
            .far_hair
            .as_ref()
            .expect("SpawnAvatar::from asks for a far tier")
            .mesh
            .positions
            .len();
        let mut query = app.world_mut().query::<(
            &HairTier,
            &Mesh3d,
            &MeshMaterial3d<StandardMaterial>,
            &SkinnedMesh,
        )>();
        let tiers: Vec<_> = query
            .iter(app.world())
            .map(|(tier, mesh, material, skin)| {
                (*tier, mesh.0.clone(), material.0.clone(), skin.clone())
            })
            .collect();
        let [near, far] = [HairTier::Near, HairTier::Far].map(|want| {
            let found: Vec<_> = tiers.iter().filter(|(tier, ..)| *tier == want).collect();
            assert_eq!(found.len(), 1, "one {want:?} tier, found {}", found.len());
            found[0].clone()
        });
        assert_eq!(far.2, near.2, "the far tier has a material of its own");
        assert_eq!(far.3.inverse_bindposes, near.3.inverse_bindposes);
        assert_eq!(far.3.joints, near.3.joints);
        assert_ne!(far.1, near.1, "the far tier draws the near mesh");
        let meshes = app.world().resource::<Assets<Mesh>>();
        let drawn = meshes.get(&far.1).expect("the far mesh uploaded");
        assert_eq!(
            drawn.count_vertices(),
            far_vertices,
            "the far entity does not draw Avatar::far_hair"
        );
    }

    #[test]
    fn a_body_built_without_a_far_tier_draws_its_hair_everywhere_as_before() {
        // The control, and the promise to a consumer who builds its own config:
        // no far tier asked for, no tier, no range, one hair entity, drawn at
        // every distance - byte for byte the entity tree 0.9 spawned.
        let mut app = app();
        app.world_mut().spawn(SpawnAvatar {
            record: AvatarRecord::new("Untiered", Archetype::default()),
            config: AvatarConfig::default(),
            closure: 0.0,
        });
        app.update();
        let mut ranges = app.world_mut().query::<&VisibilityRange>();
        assert_eq!(ranges.iter(app.world()).count(), 0);
        let mut tiers = app.world_mut().query::<&HairTier>();
        assert_eq!(tiers.iter(app.world()).count(), 0);
        let mut body = app.world_mut().query::<&AvatarBody>();
        let budget = body
            .single(app.world())
            .expect("one body")
            .avatar
            .budget
            .meshes;
        let mut meshes = app.world_mut().query::<&Mesh3d>();
        assert_eq!(meshes.iter(app.world()).count(), budget);
    }

    #[test]
    fn retuning_writes_a_range_only_when_it_moves() {
        // Every write through a `Mut` stamps the component changed, and ONE
        // changed range makes Bevy clear and rebuild its whole table of them
        // that frame - for every body in the world. So a frame with nothing to
        // retune must stamp nothing. Read the tick, not is_changed, which is
        // relative to whichever system last looked.
        let mut app = app();
        spawn(&mut app);
        let ticks = |app: &mut App| -> Vec<u32> {
            let mut query = app.world_mut().query::<(&HairTier, Ref<VisibilityRange>)>();
            query
                .iter(app.world())
                .map(|(_, range)| range.last_changed().get())
                .collect()
        };
        let before = ticks(&mut app);
        assert_eq!(before.len(), 2, "two tiers");
        app.update();
        app.update();
        assert_eq!(ticks(&mut app), before, "an idle frame stamped a range");

        // The same value written again through the resource moves nothing.
        app.insert_resource(HairLod::default());
        app.update();
        assert_eq!(
            ticks(&mut app),
            before,
            "an unchanged resource stamped a range"
        );

        // Liveness: a real change is written, and lands where it should.
        app.insert_resource(HairLod {
            switch: 30.0,
            margin: 0.0,
        });
        app.update();
        assert_ne!(ticks(&mut app), before, "a moved switch was not written");
        let mut query = app.world_mut().query::<(&HairTier, &VisibilityRange)>();
        for (tier, range) in query.iter(app.world()) {
            let wanted = HairLod {
                switch: 30.0,
                margin: 0.0,
            }
            .range(*tier);
            assert!(
                *range == wanted,
                "{tier:?} ranges {:?}..{:?}, wanted {:?}..{:?}",
                range.start_margin,
                range.end_margin,
                wanted.start_margin,
                wanted.end_margin
            );
        }
    }

    #[test]
    fn a_zero_margin_is_abrupt_and_a_band_never_divides_by_zero() {
        // Bevy skips the dither shader for abrupt ranges, which is what keeps
        // the owner's pop from costing a pipeline variant. And inside a band
        // the shader divides by the width of whichever margin the camera is
        // in, so no margin a camera can stand inside may be empty: the near
        // tier's start is the one a default would leave at 0..0.
        let pop = HairLod::default();
        assert!(pop.margin <= 0.0, "the owner's margin is 0 (#48)");
        for tier in [HairTier::Near, HairTier::Far] {
            assert!(pop.range(tier).is_abrupt(), "{tier:?} dithers at margin 0");
        }
        let band = HairLod {
            switch: 12.0,
            margin: 2.0,
        };
        let near = band.range(HairTier::Near);
        assert!(near.start_margin.end > near.start_margin.start);
        assert!(near.end_margin.end > near.end_margin.start);
        let far = band.range(HairTier::Far);
        assert!(far.start_margin.end > far.start_margin.start);
        // The two meet: the near tier fades out over exactly the band the far
        // one fades in over, which is what makes Bevy's dither patterns
        // complementary rather than leaving a gap or a double.
        assert_eq!(near.end_margin, far.start_margin);
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
        // Hair samples an image too since #47, and that image is the strand
        // mask, which is not the atlas: counted out by its handle.
        let textured = handles
            .iter()
            .filter(|handle| {
                materials.get(*handle).is_some_and(|material| {
                    material
                        .base_color_texture
                        .as_ref()
                        .is_some_and(|texture| *texture != STRAND_MASK)
                })
            })
            .count();
        assert_eq!(
            textured, expected,
            "{textured} meshes sampled the skin atlas, against {expected} that are skin"
        );
    }

    #[test]
    fn the_strand_mask_is_uploaded_once_for_every_body() {
        // #47. One image for the app, not one per body: the engine paints a
        // single mask for every avatar a process builds. Counted as what a
        // second body adds, because Bevy's image plugin keeps images of its own
        // in the same store.
        let mut app = app();
        spawn(&mut app);
        let one = app.world().resource::<Assets<Image>>().len();
        spawn(&mut app);
        let images = app.world().resource::<Assets<Image>>();
        assert_eq!(
            images.len() - one,
            3,
            "a second body uploaded {} images, not its skin's three",
            images.len() - one
        );
        let mask = images
            .get(&STRAND_MASK)
            .expect("the strand mask was uploaded");
        assert_eq!(
            mask.data.as_deref(),
            Some(symbios_avatar::strand_mask().rgba.as_slice()),
            "the uploaded mask is not the one the engine painted"
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
        let atlas = SkinMaps {
            albedo: Handle::default(),
            normal: Handle::default(),
            orm: Handle::default(),
        };
        let apart = material_for(MeshKind::Hair, &atlas).perceptual_roughness
            - material_for(MeshKind::Skin, &atlas).perceptual_roughness;
        assert!(
            apart.abs() > 0.1,
            "hair and skin shade {apart} apart, which is not apart"
        );
    }

    /// Every kind of mesh a body draws, for asking each the same question.
    const KINDS: [MeshKind; 4] = [
        MeshKind::Skin,
        MeshKind::Hair,
        MeshKind::Cloth,
        MeshKind::Eye,
    ];

    /// Maps that point at no image: no question below is about which image a
    /// material samples.
    fn unmapped() -> SkinMaps {
        SkinMaps {
            albedo: Handle::default(),
            normal: Handle::default(),
            orm: Handle::default(),
        }
    }

    #[test]
    fn a_hair_card_is_drawn_from_both_sides_and_nothing_else_is() {
        // #46. A lock is a single-sided card turned to face outward, and a
        // culled card vanishes from behind and edge-on: the inside of a
        // curtain, the far side of a lock beside the face. Both fields,
        // because each is half of it - `double_sided` turns a back face's
        // normal round to the viewer, and `cull_mode` is what stops the face
        // being thrown away first. A material with one and not the other draws
        // nothing new, or draws it lit from the wrong side.
        //
        // And only hair: cards are what the engine documents as single-sided
        // for a consumer to draw from both sides, and widening it to another
        // kind would change that kind's look with no sheet to say so.
        let maps = unmapped();
        for kind in KINDS {
            let material = material_for(kind, &maps);
            let cards = kind == MeshKind::Hair;
            assert_eq!(material.double_sided, cards, "{kind:?} double_sided");
            assert_eq!(
                material.cull_mode,
                (!cards).then_some(bevy::render::render_resource::Face::Back),
                "{kind:?} cull_mode"
            );
        }
    }

    #[test]
    fn a_hair_card_is_cut_out_of_the_strand_mask_and_nothing_else_is() {
        // #47. A card's end is a flat cap and a row of them is a picket fence,
        // so hair samples the engine's strand mask and is not drawn below half
        // its alpha. Masked, never blended: a mask needs no sorting, and the
        // shadow pass honours it. And only hair - skin samples its atlas, and
        // nothing else on a body has a silhouette of its own to cut.
        let maps = unmapped();
        for kind in KINDS {
            let material = material_for(kind, &maps);
            let cards = kind == MeshKind::Hair;
            let cut = match material.alpha_mode {
                AlphaMode::Mask(at) => (at - 0.5).abs() < 1e-6,
                _ => false,
            };
            assert_eq!(
                cut, cards,
                "{kind:?} alpha_mode is {:?}",
                material.alpha_mode
            );
            if !cards {
                assert!(
                    matches!(material.alpha_mode, AlphaMode::Opaque),
                    "{kind:?} alpha_mode is {:?}",
                    material.alpha_mode
                );
            }
            let masked = material.base_color_texture.as_ref() == Some(&STRAND_MASK);
            assert_eq!(masked, cards, "{kind:?} samples the strand mask: {masked}");
        }
    }

    #[test]
    fn no_material_asks_for_anisotropy() {
        // #46, and both ways of turning it on are closed. Bevy 0.19 builds the
        // anisotropy frame only when bevy_pbr carries its
        // pbr_anisotropy_texture feature, which this crate does not enable -
        // yet a non-zero strength still switches the anisotropic lighting on
        // without it, through a zero tangent, and on the #46 sheet every head
        // of hair came out blown-out white. Enabling the feature is the other
        // way, and it binds one more texture on every StandardMaterial in the
        // consuming app: overlands' terrain material already sits at WebGL2's
        // sixteen, where one texture more once panicked pipeline creation
        // (overlands #245).
        //
        // Asked the way Bevy asks it: the anisotropic lighting is keyed on a
        // strength above zero.
        let maps = unmapped();
        for kind in KINDS {
            let strength = material_for(kind, &maps).anisotropy_strength;
            assert!(
                strength <= 0.0,
                "{kind:?} asks for anisotropy at {strength}"
            );
        }
    }
}
