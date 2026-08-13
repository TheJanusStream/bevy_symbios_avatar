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
//! **Colour space.** The engine authors colour in **sRGB** — every one of them,
//! the atlas and the vertex colours alike. `symbios_avatar::texture::skin` says
//! so in as many words for the melanin ramp, the hair ramp and `dress::dye` are
//! picked by eye in the same space, and the engine's own software renderer
//! decodes vertex colours with `to_linear` before it lights them.
//!
//! Bevy wants linear on both channels, and gets there two different ways:
//! `Rgba8UnormSrgb` decodes the atlas on sample, and `ATTRIBUTE_COLOR` is taken
//! as linear already — so the atlas is handed over raw and the vertex colours
//! have to be decoded here, by `linear_of`.
//!
//! Getting this wrong does not produce an obvious error, and this crate has
//! paid for it once already (#14, on the word of an upstream doc comment that
//! called the vertex colours linear): every vertex-coloured thing on the body
//! — the iris, the hair, the garment — drew two to four times too bright in
//! the midtones here and correctly in the software renderer. It was the eyes
//! that showed it, because a mid-blue iris undecoded is a pale grey-blue bead;
//! skin never showed it at all, because skin comes through the atlas, which
//! both instruments always decoded. A body that is slightly the wrong colour
//! in one renderer is exactly the kind of difference that gets blamed on
//! lighting — and was.
//!
//! **Normals.** A mesh that carries explicit normals gets them, and one that
//! does not has them derived. The engine is deliberate about which is which:
//! after a UV seam split, neighbouring copies of a vertex are no longer joined,
//! so deriving normals would crease the surface along every seam. Seam normals
//! reading as a skinning defect is one of the two false diagnoses this crate
//! exists to prevent, so it is not a detail to get casually right.

use bevy::asset::RenderAssetUsages;
use bevy::color::{LinearRgba, Srgba};
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
        // Decoded, not copied: see the module note on colour space. Alpha is 1:
        // nothing on a body is transparent, and hair is a swept solid precisely
        // so it need not be.
        mesh.insert_attribute(
            Mesh::ATTRIBUTE_COLOR,
            source
                .colours
                .iter()
                .map(|c| linear_of(*c))
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

    // A normal-mapped material needs tangents, and Bevy will not invent them
    // at draw time. Generated wherever the mesh carries UVs — which is the
    // atlas-covered skin, the one mesh whose material carries a normal map.
    // A mesh whose UVs defeat mikktspace keeps its smooth normals and says so,
    // rather than shipping a broken frame.
    if source.uvs.len() == vertices
        && let Err(error) = mesh.generate_tangents()
    {
        bevy::log::warn!("skin tangents could not be generated: {error}");
    }
    mesh
}

/// One of the engine's sRGB colours, as the linear RGBA Bevy's vertex colours
/// want.
///
/// Through Bevy's own `Srgba`-to-`LinearRgba` conversion rather than a transfer
/// function written out here: the curve has a linear toe below 0.04045 that a bare
/// `powf(2.2)` misses, and the one place it could disagree with the GPU's
/// decode of the atlas beside it is a hand-rolled copy of it.
#[must_use]
fn linear_of(colour: symbios_avatar::Vec3) -> [f32; 4] {
    let linear = LinearRgba::from(Srgba::new(colour.x, colour.y, colour.z, 1.0));
    [linear.red, linear.green, linear.blue, linear.alpha]
}

/// Uploads a painted skin atlas's albedo as a texture.
///
/// One of three: [`normal_image`] and [`orm_image`] carry the maps the engine
/// paints beside it, and since engine #45 both renderers consume all three —
/// the two instruments are looking at the same picture.
///
/// `Rgba8UnormSrgb`, because the engine's albedo is sRGB-encoded and a texture
/// declared linear renders a body noticeably pale.
#[must_use]
pub fn atlas_image(map: &symbios_avatar::TextureMap) -> Image {
    image_of(&map.albedo, map, TextureFormat::Rgba8UnormSrgb)
}

/// Uploads the painted skin's tangent-space normal map as a texture.
///
/// `Rgba8Unorm`, never sRGB: a normal map is data, and running its bytes
/// through a colour transfer curve skews every slope toward the surface —
/// relief that quietly weakens is far harder to notice than relief that is
/// missing (#22).
#[must_use]
pub fn normal_image(map: &symbios_avatar::TextureMap) -> Image {
    image_of(&map.normal, map, TextureFormat::Rgba8Unorm)
}

/// Uploads the painted skin's ORM map as a texture.
///
/// One image serves two material slots: Bevy reads G and B for
/// `metallic_roughness_texture` and R for `occlusion_texture`, which is
/// exactly the layout the engine bakes. Linear, like every data map.
#[must_use]
pub fn orm_image(map: &symbios_avatar::TextureMap) -> Image {
    image_of(&map.roughness, map, TextureFormat::Rgba8Unorm)
}

/// One of the map's pixel buffers, as a single-level texture.
fn image_of(pixels: &[u8], map: &symbios_avatar::TextureMap, format: TextureFormat) -> Image {
    let level = (map.width * map.height * 4) as usize;
    // Only the base level: the map may carry mips laid out contiguously after
    // it, and handing the whole buffer to a single-level texture is a size
    // mismatch rather than a free upgrade.
    let base = pixels.get(..level).unwrap_or(pixels).to_vec();
    Image::new(
        Extent3d {
            width: map.width,
            height: map.height,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        base,
        format,
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
    fn the_skin_carries_tangents_and_all_three_maps_upload() {
        // A normal-mapped material without ATTRIBUTE_TANGENT fails at draw
        // time, a long way from the conversion that forgot it; and a data map
        // uploaded sRGB skews every slope toward the surface — relief that
        // quietly weakens, which no test of "is the texture there" can see.
        let avatar = avatar();
        let body = avatar
            .drawn(0.0)
            .into_iter()
            .find(|mesh| mesh.kind == MeshKind::Skin)
            .expect("a body has skin");
        let mesh = mesh_of(&body);
        assert_eq!(
            count(&mesh, Mesh::ATTRIBUTE_TANGENT),
            Some(body.mesh.vertex_count()),
            "the skin mesh must carry tangents for its normal map"
        );

        let level = (avatar.skin.width * avatar.skin.height * 4) as usize;
        for (name, image, format) in [
            (
                "albedo",
                atlas_image(&avatar.skin),
                TextureFormat::Rgba8UnormSrgb,
            ),
            (
                "normal",
                normal_image(&avatar.skin),
                TextureFormat::Rgba8Unorm,
            ),
            ("orm", orm_image(&avatar.skin), TextureFormat::Rgba8Unorm),
        ] {
            assert_eq!(image.texture_descriptor.format, format, "{name} format");
            assert_eq!(
                image.data.as_ref().map(Vec::len),
                Some(level),
                "{name} carries exactly the base level"
            );
        }
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
        //
        // **The gradient has to be ASKED FOR now, and the test failing is what
        // said so** (symbios-avatar #202/#209). A record carries a root colour
        // and a tip colour per region, and the default record sets both to the
        // same melanin stop — so a default head of hair really is one colour,
        // and this test was passing on a distinctness the shell era got from a
        // per-lock shade that no longer exists. Asserting a gradient survives
        // conversion means writing one into the record first; asserting it on
        // whatever the default happens to carry is asserting the default.
        let mut record = AvatarRecord::new("Converted", Archetype::default());
        record.hair.scalp.roots = [0.05, 0.03, 0.02];
        record.hair.scalp.tips = [0.72, 0.55, 0.28];
        record.sanitize();
        let avatar = Avatar::build(&record).expect("a biped builds");
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
    fn vertex_colours_are_decoded_from_srgb_rather_than_copied() {
        // #14. The engine authors colour in sRGB and Bevy
        // wants ATTRIBUTE_COLOR linear, so a copy is 2-4x too bright in the
        // midtones — which is what made a mid-blue iris read as a pale bead
        // here and as a dark eye in the software renderer.
        //
        // Asserted against the transfer function's own arithmetic rather than
        // against a remembered pixel: the toe below 0.04045 is linear and the
        // shoulder is a 2.4 power, and a hand-rolled 2.2 gamma passes a test
        // written from a screenshot while still disagreeing with the GPU's
        // decode of the atlas beside it.
        let avatar = avatar();
        let hair = avatar
            .drawn(0.0)
            .into_iter()
            .find(|mesh| mesh.kind == MeshKind::Hair)
            .expect("a biped has hair");
        let mesh = mesh_of(&hair);
        let Some(VertexAttributeValues::Float32x4(converted)) =
            mesh.attribute(Mesh::ATTRIBUTE_COLOR)
        else {
            panic!("hair converted without its colours");
        };
        assert_eq!(converted.len(), hair.mesh.colours.len());

        let expected = |channel: f32| {
            if channel <= 0.040_45 {
                channel / 12.92
            } else {
                ((channel + 0.055) / 1.055).powf(2.4)
            }
        };
        let mut darkened = 0;
        for (source, drawn) in hair.mesh.colours.iter().zip(converted) {
            for (authored, linear) in [source.x, source.y, source.z].into_iter().zip(drawn) {
                assert!(
                    (linear - expected(authored)).abs() < 1e-5,
                    "{authored} decoded to {linear}, not {}",
                    expected(authored)
                );
                if authored > 0.05 && *linear < authored * 0.9 {
                    darkened += 1;
                }
            }
        }
        assert!(
            darkened > 0,
            "nothing was decoded at all: every channel came through unchanged"
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
