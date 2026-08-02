//! Turning the engine's geometry into Bevy's.
//!
//! Two conversions, and both of them are places where the two instruments could
//! silently disagree, so both are spelled out rather than left to a reader to
//! infer.
//!
//! The engine's [`PolyMesh`] is a polygon soup with per-vertex attributes: faces
//! are rings of any length, and it carries positions, UVs, normals, bone
//! influences and linear-space colours. A Bevy [`Mesh`] is a triangle list with
//! parallel attribute arrays. The conversion is therefore mechanical except for
//! two decisions.
//!
//! **Colour space.** The engine's vertex colours are linear and the atlas it
//! paints is sRGB-encoded. Bevy's `ATTRIBUTE_COLOR` is linear and
//! `Rgba8UnormSrgb` decodes on sample, so each is handed over in the space it
//! is already in. Getting this wrong does not produce an obvious error; it
//! produces a body that is slightly the wrong colour in one renderer, which is
//! exactly the kind of difference that gets blamed on lighting.
//!
//! **Normals.** A mesh that carries explicit normals gets them, and one that
//! does not has them derived. The engine is deliberate about which is which:
//! after a UV seam split, neighbouring copies of a vertex are no longer joined,
//! so deriving normals would crease the surface along every seam. Seam normals
//! reading as a skinning defect is one of the two false diagnoses this crate
//! exists to prevent, so it is not a detail to get casually right.

use bevy::asset::RenderAssetUsages;
use bevy::image::Image;
use bevy::mesh::{Indices, Mesh, PrimitiveTopology, VertexAttributeValues};
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use symbios_avatar::{AvatarMesh, PolyMesh};

/// Converts one of the engine's meshes into a Bevy mesh.
///
/// The result is a triangle list carrying whatever the source had: positions
/// always, then normals, UVs, colours and skin influences where they exist.
///
/// Skin influences become the pair of attributes Bevy's skinning shader wants,
/// `ATTRIBUTE_JOINT_INDEX` and `ATTRIBUTE_JOINT_WEIGHT`. Both are emitted
/// whenever the mesh is skinned at all, because Bevy needs them together — a
/// mesh with one and not the other fails at draw time, a long way from where
/// the mistake was made.
#[must_use]
pub fn mesh_of(source: &AvatarMesh) -> Mesh {
    polymesh_to_bevy(&source.mesh)
}

/// The conversion itself, for a bare mesh with no kind attached.
#[must_use]
pub fn polymesh_to_bevy(source: &PolyMesh) -> Mesh {
    let vertices = source.positions.len();
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        // Kept on the CPU as well as uploaded: this crate is here to be
        // compared against another renderer, and a mesh that has been thrown
        // away cannot be measured.
        RenderAssetUsages::default(),
    );

    mesh.insert_attribute(
        Mesh::ATTRIBUTE_POSITION,
        source
            .positions
            .iter()
            .map(|p| [p.x, p.y, p.z])
            .collect::<Vec<_>>(),
    );

    // Explicit where the engine kept them, derived where it did not. See the
    // module note: deriving across a seam split creases the surface.
    if source.normals.len() == vertices {
        mesh.insert_attribute(
            Mesh::ATTRIBUTE_NORMAL,
            source
                .normals
                .iter()
                .map(|n| [n.x, n.y, n.z])
                .collect::<Vec<_>>(),
        );
    }

    if source.uvs.len() == vertices {
        mesh.insert_attribute(
            Mesh::ATTRIBUTE_UV_0,
            source.uvs.iter().map(|uv| [uv.x, uv.y]).collect::<Vec<_>>(),
        );
    }

    if source.colours.len() == vertices {
        // Already linear on both sides. Alpha is 1: nothing on a body is
        // transparent, and hair is a swept solid precisely so it need not be.
        mesh.insert_attribute(
            Mesh::ATTRIBUTE_COLOR,
            source
                .colours
                .iter()
                .map(|c| [c.x, c.y, c.z, 1.0])
                .collect::<Vec<_>>(),
        );
    }

    if source.skin.len() == vertices {
        let mut indices = Vec::with_capacity(vertices);
        let mut weights = Vec::with_capacity(vertices);
        for influences in &source.skin {
            let mut index = [0u16; 4];
            let mut weight = [0f32; 4];
            for (slot, influence) in influences.iter().enumerate().take(4) {
                index[slot] = influence.joint;
                weight[slot] = influence.weight;
            }
            indices.push(index);
            weights.push(weight);
        }
        mesh.insert_attribute(
            Mesh::ATTRIBUTE_JOINT_INDEX,
            VertexAttributeValues::Uint16x4(indices),
        );
        mesh.insert_attribute(Mesh::ATTRIBUTE_JOINT_WEIGHT, weights);
    }

    mesh.insert_indices(Indices::U32(
        source.triangulated().into_iter().flatten().collect(),
    ));

    if source.normals.len() != vertices {
        mesh.compute_normals();
    }
    mesh
}

/// Uploads a painted skin atlas as a texture.
///
/// The albedo channel only. The engine paints a normal and a roughness map
/// beside it, and a viewer that wants physically-lit skin should take those
/// too; this takes what the software renderer takes, so the two instruments are
/// looking at the same picture.
///
/// `Rgba8UnormSrgb`, because the engine's albedo is sRGB-encoded and a texture
/// declared linear renders a body noticeably pale.
#[must_use]
pub fn atlas_image(map: &symbios_avatar::TextureMap) -> Image {
    let level = (map.width * map.height * 4) as usize;
    // Only the base level: the map may carry mips laid out contiguously after
    // it, and handing the whole buffer to a single-level texture is a size
    // mismatch rather than a free upgrade.
    let base = map.albedo.get(..level).unwrap_or(&map.albedo).to_vec();
    Image::new(
        Extent3d {
            width: map.width,
            height: map.height,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        base,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::mesh::MeshVertexAttribute;
    use symbios_avatar::{Archetype, Avatar, AvatarRecord, MeshKind};

    fn avatar() -> Avatar {
        Avatar::build(&AvatarRecord::new("Converted", Archetype::default()))
            .expect("a biped builds")
    }

    fn count(mesh: &Mesh, attribute: MeshVertexAttribute) -> Option<usize> {
        mesh.attribute(attribute).map(VertexAttributeValues::len)
    }

    #[test]
    fn every_mesh_an_avatar_draws_converts() {
        // The conformance half of this crate: whatever the engine hands over
        // has to be drawable. If it returns something that cannot be converted,
        // this is where that shows — the same role the software renderer plays
        // for the engine's build path.
        let avatar = avatar();
        for drawn in avatar.drawn(0.0) {
            let mesh = mesh_of(&drawn);
            let vertices = drawn.mesh.vertex_count();
            assert_eq!(
                count(&mesh, Mesh::ATTRIBUTE_POSITION),
                Some(vertices),
                "{:?} lost vertices",
                drawn.kind
            );
            assert_eq!(
                count(&mesh, Mesh::ATTRIBUTE_NORMAL),
                Some(vertices),
                "{:?} has no normals",
                drawn.kind
            );
            assert_eq!(
                mesh.indices().map(Indices::len),
                Some(drawn.mesh.triangulated().len() * 3),
                "{:?} lost triangles",
                drawn.kind
            );
        }
    }

    #[test]
    fn a_skinned_mesh_carries_both_joint_attributes() {
        // Bevy needs the pair. A mesh with indices and no weights builds no
        // pipeline, and it fails at draw time rather than here.
        let avatar = avatar();
        let body = avatar
            .drawn(0.0)
            .into_iter()
            .find(|mesh| mesh.kind == MeshKind::Skin)
            .expect("a body has skin");
        let mesh = mesh_of(&body);
        let vertices = body.mesh.vertex_count();
        assert_eq!(count(&mesh, Mesh::ATTRIBUTE_JOINT_INDEX), Some(vertices));
        assert_eq!(count(&mesh, Mesh::ATTRIBUTE_JOINT_WEIGHT), Some(vertices));
    }

    #[test]
    fn every_joint_index_names_a_joint_of_the_rig() {
        // An index past the end of the palette is not a visible defect. It is a
        // vertex skinned to whatever happens to follow it.
        let avatar = avatar();
        let joints = avatar.rig.len();
        for drawn in avatar.drawn(0.0) {
            for influences in &drawn.mesh.skin {
                for influence in influences {
                    assert!(
                        (influence.joint as usize) < joints,
                        "{:?} names joint {} of {joints}",
                        drawn.kind,
                        influence.joint
                    );
                }
            }
        }
    }

    #[test]
    fn hair_keeps_its_per_lock_colour_through_the_conversion() {
        // The engine merges every lock into one draw and distinguishes them on
        // the vertices. Dropping the colours would cost nothing measurable and
        // turn the hair into a helmet — a defect this crate would then be
        // blamed for.
        let avatar = avatar();
        let hair = avatar
            .drawn(0.0)
            .into_iter()
            .find(|mesh| mesh.kind == MeshKind::Hair)
            .expect("a biped has hair");
        let mesh = mesh_of(&hair);
        let Some(VertexAttributeValues::Float32x4(colours)) = mesh.attribute(Mesh::ATTRIBUTE_COLOR)
        else {
            panic!("hair converted without its colours");
        };
        let mut distinct: Vec<[u32; 3]> = colours
            .iter()
            .map(|c| [c[0].to_bits(), c[1].to_bits(), c[2].to_bits()])
            .collect();
        distinct.sort_unstable();
        distinct.dedup();
        assert!(
            distinct.len() > 8,
            "every lock came out the same colour ({} distinct)",
            distinct.len()
        );
    }

    #[test]
    fn the_atlas_uploads_at_the_size_it_was_painted() {
        let avatar = avatar();
        let image = atlas_image(&avatar.skin);
        assert_eq!(image.width(), avatar.skin.width);
        assert_eq!(image.height(), avatar.skin.height);
        assert_eq!(
            image.texture_descriptor.format,
            TextureFormat::Rgba8UnormSrgb
        );
        assert_eq!(
            image.data.as_ref().map(Vec::len),
            Some((avatar.skin.width * avatar.skin.height * 4) as usize),
            "the atlas uploaded a different number of texels than it has"
        );
    }
}
