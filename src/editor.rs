//! A panel for every axis a record can hold, and nothing else.
//!
//! The viewer's own doc argues against a panel — "a body, a light, and a camera
//! you can walk round it with. Nothing else, on purpose" — and that argument is
//! right. This does not break it, and the two rules that keep it from breaking
//! it are the whole design:
//!
//! **The record, and only the record.** Every control here writes a field of an
//! [`AvatarRecord`]. There is no slider for a skull knot, a face pass, or a
//! relief coefficient, because none of those is in a record: a body tuned
//! against them cannot be saved, shared, or rebuilt by anyone else, and an
//! afternoon spent perfecting one would produce nothing anybody could hold. If
//! an engine constant deserves an axis, the answer is to give it one in the
//! engine.
//!
//! **A judgement image is never a screenshot of a UI.** The panel hides on a
//! key, and it never draws at all under `--shot`. What is photographed is a
//! body, a light and a camera, exactly as before.
//!
//! ## What it costs, measured rather than assumed
//!
//! A rebuild is not free and cannot be hidden. On the shipped body, release,
//! 24,398 triangles:
//!
//! ```text
//!   full build, atlas 1024   277.0 ms     skull measure       16.6 ms
//!   full build, atlas  512   103.9 ms     ... and again       16.9 ms
//!   full build, atlas  256    68.4 ms     everything else     20.9 ms
//!   geometry only, atlas 32   54.4 ms
//!   build_body alone           5.5 ms
//! ```
//!
//! So an axis cannot drive a rebuild per frame, and skipping the atlas is not
//! enough on its own — two thirds of what is left is the skull being measured
//! twice, which [`symbios_avatar`] issue #89 covers and this crate must not.
//!
//! What follows is [`DRAFT_ATLAS`]: while an axis is moving, rebuild at 256 as
//! often as frames allow, which is about fourteen a second; once nothing has
//! changed for [`SETTLE`], rebuild once at the full atlas so the complexion is
//! the one that ships. The atlas is not held across the draft, because it
//! cannot be — the charts are derived from the body that changed.
//!
//! ## Emitting the record
//!
//! The button that makes the panel worth more than a set of sliders. "The chin
//! looks wrong on the one I was fiddling with" becomes a record anybody can
//! rebuild. Values are quantised by [`AvatarRecord::sanitize`] on every edit —
//! not by arithmetic repeated here — because the wire format is scaled integers
//! in thousandths and has no floats at all, so a slider reading 0.4567 would be
//! showing a number no record can hold.

use std::time::Duration;

use bevy::mesh::skinning::SkinnedMeshInverseBindposes;
use bevy::platform::time::Instant;
use bevy::prelude::*;
use bevy_egui::{EguiContexts, EguiPrimaryContextPass, egui};
use symbios_avatar::{
    Archetype, Avatar, AvatarConfig, AvatarRecord, Category, HumanoidParams, Leg, Limb,
    QuadrupedParams, Rig, Sleeve, Zone,
};

use crate::spawn::{AvatarBody, spawn_avatar};

/// Atlas side used while an axis is moving.
///
/// 68 ms a rebuild against 277 at the full size. Below this the saving is 14 ms
/// and the complexion stops being a complexion, so this is where the ladder
/// bottoms out usefully.
pub const DRAFT_ATLAS: u32 = 256;

/// How long an axis must be still before the body is rebuilt at full size.
pub const SETTLE: Duration = Duration::from_millis(250);

/// The avatar the panel edits.
///
/// Rebuilding despawns the entity and asks for a new one, exactly as a re-roll
/// does, because every mesh, chart and weight belongs to the body that changed.
#[derive(Component, Debug, Default, Clone, Copy)]
pub struct EditedAvatar;

/// Where the panel's JSON box stands relative to the record.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Json {
    /// Showing the record, and following it as it changes.
    #[default]
    Following,
    /// Somebody has typed in it, so it is theirs until they load or discard it.
    Edited,
}

/// The aperture measurement, which takes about two seconds and blocks.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
enum Measuring {
    /// Nothing asked for.
    #[default]
    Idle,
    /// Asked for, and waiting a frame so the panel can say so before it blocks.
    Asked,
    /// The last reading, as lines to show.
    Done(Vec<String>),
}

/// The record being edited, and everything the panel needs to edit it.
#[derive(Resource)]
pub struct RecordEditor {
    /// The record. Every control writes here and nowhere else.
    pub record: AvatarRecord,
    /// Whether the panel draws at all.
    pub open: bool,
    /// Set when a control changed the record and the body has not caught up.
    dirty: bool,
    /// Whether the body on screen was built at [`DRAFT_ATLAS`].
    draft: bool,
    /// How long since the last edit, for [`SETTLE`].
    still: Duration,
    /// How long the last build took, and at what atlas.
    last_build: Option<(Duration, u32)>,
    /// How long the panel itself took last frame.
    ///
    /// The viewer presents on vsync, so a frame delta cannot show what a panel
    /// costs — it would read 16.7 ms whether the panel cost 0.2 ms or 10. This
    /// times only the panel.
    last_panel: Duration,
    /// The JSON box's contents.
    json: String,
    /// Whether that box is following the record or has been typed in.
    json_state: Json,
    /// The share-code box's contents.
    ///
    /// Held rather than derived, because this box is two things at once: what
    /// this record's look renders to, and somewhere to paste one from a friend.
    /// A field that recomputed itself every frame could not be typed into.
    code: String,
    /// Whether that box is showing this record's code or one somebody pasted.
    code_state: Json,
    /// What the last load or copy did.
    status: String,
    /// The aperture reading.
    measuring: Measuring,
}

impl RecordEditor {
    /// An editor holding `record`, with the panel open.
    #[must_use]
    pub fn new(record: AvatarRecord) -> Self {
        let json = to_json(&record);
        let code = record.share_code();
        Self {
            record,
            open: true,
            // The first body is this system's too, so it is timed like every
            // other and the panel and the body cannot start out of step.
            dirty: true,
            draft: false,
            still: Duration::ZERO,
            last_build: None,
            last_panel: Duration::ZERO,
            json,
            json_state: Json::Following,
            code,
            code_state: Json::Following,
            status: String::new(),
            measuring: Measuring::Idle,
        }
    }

    /// Marks the record as changed, so the body is rebuilt.
    ///
    /// Sanitises first, which is what snaps every axis to the thousandth the
    /// wire format stores. Calling it is how a caller outside the panel — a key
    /// binding, a test — edits the record without knowing any of that.
    pub fn touched(&mut self) {
        self.record.sanitize();
        self.dirty = true;
        self.still = Duration::ZERO;
        self.restate();
    }

    /// Refreshes the emitted JSON for a change that is not a change of body.
    ///
    /// A name, a seed and a lock are all in the record and none of them alters
    /// a vertex. Rebuilding for them would spend 68 ms per keystroke typing a
    /// name, and not refreshing the JSON would emit a record that is not the
    /// one on screen.
    pub fn restate(&mut self) {
        if self.json_state == Json::Following {
            self.json = to_json(&self.record);
        }
        // The share code follows the record on exactly the same terms, and for
        // the same reason: a code shown beside a body that is no longer the one
        // it names is worse than no code at all, because somebody will read it
        // out. Both boxes stop following the moment they are typed in.
        if self.code_state == Json::Following {
            self.code = self.record.share_code();
        }
    }

    /// Draws new values for every unlocked category and rebuilds.
    pub fn reroll(&mut self, seed: i64) {
        self.record.reroll(seed);
        self.touched();
    }

    /// How long the last build took, and the atlas it was built at.
    #[must_use]
    pub fn last_build(&self) -> Option<(Duration, u32)> {
        self.last_build
    }

    /// How long the panel itself took to draw last frame.
    #[must_use]
    pub fn last_panel(&self) -> Duration {
        self.last_panel
    }
}

impl Default for RecordEditor {
    fn default() -> Self {
        Self::new(AvatarRecord::new("Viewed", Archetype::default()))
    }
}

/// The panel, and the rebuild it drives.
///
/// Insert a [`RecordEditor`] to choose the record it starts from; the default
/// is an unrolled humanoid. Spawn nothing: this plugin asks for the first body
/// itself, so the record the panel shows and the body on screen are the same
/// thing from the first frame.
#[derive(Debug, Default, Clone, Copy)]
pub struct RecordEditorPlugin;

impl Plugin for RecordEditorPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<RecordEditor>()
            // A rebuild destroys a body and makes another, which is exactly
            // what [`crate::AvatarSystems::Build`] is for: nothing downstream
            // should ever hold the entity this despawned.
            .add_systems(
                Update,
                rebuild_edited_avatar.in_set(crate::AvatarSystems::Build),
            )
            .add_systems(EguiPrimaryContextPass, record_editor_panel);
    }
}

/// Rebuilds the edited body when the record has changed.
///
/// Two rebuilds, not one. A draft goes up as fast as frames allow so an axis
/// can be watched across its range; the full one lands once the axis has been
/// still for [`SETTLE`], because the complexion at [`DRAFT_ATLAS`] is not the
/// complexion that ships and a judgement made on it would be a judgement about
/// a texture nobody will see.
///
/// It builds and draws the body itself rather than asking [`SpawnAvatar`] for
/// one. Two reasons, and the second is the important one: a request would be
/// built on the *next* frame, so the timing here would either miss the build
/// entirely or have to be inferred from a frame delta — the exact instrument
/// failure this issue set out to avoid.
///
/// [`SpawnAvatar`]: crate::spawn::SpawnAvatar
#[expect(
    clippy::too_many_arguments,
    reason = "four asset stores and a body; the same argument as spawn_avatar"
)]
pub fn rebuild_edited_avatar(
    mut commands: Commands,
    mut editor: ResMut<RecordEditor>,
    time: Res<Time>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut bindposes: ResMut<Assets<SkinnedMeshInverseBindposes>>,
    edited: Query<Entity, With<EditedAvatar>>,
) {
    if !editor.dirty {
        editor.still += time.delta();
        // A draft is a stand-in, so the full build is owed even though nothing
        // has been touched since it went up.
        if !editor.draft || editor.still < SETTLE {
            return;
        }
    }

    let full = !editor.dirty;
    let config = AvatarConfig {
        atlas: if full {
            AvatarConfig::default().atlas
        } else {
            DRAFT_ATLAS
        },
        ..AvatarConfig::default()
    };

    let at = Instant::now();
    let built = Avatar::build_with(&editor.record, &config);
    let took = at.elapsed();

    let Some(avatar) = built else {
        // A record that describes no body is a record, not a crash — and a
        // panel is exactly where one gets made, by driving two limbs into each
        // other. Say so and keep the body that is up.
        editor.dirty = false;
        editor.status =
            String::from("that record describes no body; showing the last one that did");
        return;
    };

    for entity in &edited {
        // Despawned and rebuilt rather than patched, exactly as a re-roll is:
        // every mesh, chart and weight belongs to the body that changed.
        commands.entity(entity).despawn();
    }
    let root = commands.spawn((EditedAvatar, Transform::default())).id();
    spawn_avatar(
        &mut commands,
        root,
        avatar,
        0.0,
        &mut meshes,
        &mut materials,
        &mut images,
        &mut bindposes,
    );

    editor.dirty = false;
    editor.draft = !full;
    editor.still = Duration::ZERO;
    editor.last_build = Some((took, config.atlas));
}

/// Draws the panel.
///
/// Times itself, because nothing else can: the viewer presents on vsync and a
/// frame delta would read the same whatever this costs.
pub fn record_editor_panel(
    mut contexts: EguiContexts,
    mut editor: ResMut<RecordEditor>,
    bodies: Query<&AvatarBody>,
) {
    if !editor.open {
        return;
    }
    let Ok(ctx) = contexts.ctx_mut() else {
        return;
    };
    let at = Instant::now();

    // Asked for last frame, drawn once with "measuring" on screen, and now paid
    // for. Two seconds is too long to spend without having said so first.
    if editor.measuring == Measuring::Asked {
        editor.measuring = Measuring::Done(measure_aperture(bodies.iter().next()));
    }

    // Two kinds of change, because they cost three orders of magnitude apart.
    // A body change is 68 ms; a name, a seed or a lock changes the record and
    // not one vertex, and rebuilding for it would spend 68 ms per keystroke.
    let mut rebuild = false;
    let mut restate = false;
    egui::SidePanel::left("record")
        .default_width(320.0)
        .show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                let (built, noted) = identity(ui, &mut editor);
                rebuild |= built;
                restate |= noted;
                ui.separator();
                // The high tier first and open, the low tier below and shut.
                // That order is the parameterisation's, not a layout
                // preference: a composite says something about the person and
                // fans out to a dozen quantities, and the axes under it are
                // corrections to what it derived. Showing them the other way
                // round invites somebody to hand-build from the offsets a body
                // one slider up would have given them.
                rebuild |= composite_axes(ui, &mut editor.record);
                ui.separator();
                rebuild |= body_axes(ui, &mut editor.record.archetype);
                rebuild |= skin_axes(ui, &mut editor.record);
                rebuild |= eye_axes(ui, &mut editor.record);
                rebuild |= face_axes(ui, &mut editor.record);
                rebuild |= hair_axes(ui, &mut editor.record);
                rebuild |= outfit_axes(ui, &mut editor.record);
                ui.separator();
                derived(ui, &editor.record);
                ui.separator();
                share(ui, &mut editor);
                wire(ui, &mut editor);
                ui.separator();
                readout(ui, &mut editor, &bodies);
            });
        });

    if rebuild {
        editor.touched();
    } else if restate {
        editor.record.sanitize();
        editor.restate();
    }
    editor.last_panel = at.elapsed();
}

/// Name, seed, archetype and the locks a re-roll honours.
///
/// Returns whether the body changed, and whether the record changed without
/// the body changing.
fn identity(ui: &mut egui::Ui, editor: &mut RecordEditor) -> (bool, bool) {
    let mut changed = false;
    let mut noted = false;
    ui.horizontal(|ui| {
        ui.label("name");
        noted |= ui
            .add(egui::TextEdit::singleline(&mut editor.record.name).desired_width(200.0))
            .changed();
    });

    ui.horizontal(|ui| {
        ui.label("body");
        let humanoid = matches!(editor.record.archetype, Archetype::Humanoid(_));
        if ui.selectable_label(humanoid, "humanoid").clicked() && !humanoid {
            editor.record.archetype = Archetype::Humanoid(HumanoidParams::default());
            changed = true;
        }
        let quadruped = matches!(editor.record.archetype, Archetype::Quadruped(_));
        if ui.selectable_label(quadruped, "quadruped").clicked() && !quadruped {
            editor.record.archetype = Archetype::Quadruped(QuadrupedParams::default());
            changed = true;
        }
    });

    ui.horizontal(|ui| {
        ui.label("seed");
        // Typing a seed records which draw a body came from; it does not
        // re-draw one. That is the button beside it.
        noted |= ui
            .add(egui::DragValue::new(&mut editor.record.seed).speed(1.0))
            .changed();
        // **A hunt, not a shuffle** (#122). Deliberately the neighbouring seed
        // rather than one off the clock: this is an instrument, and a seed you
        // cannot go back to is a body you cannot show anybody — the viewer's
        // first re-roll button drew off the clock precisely because it had
        // nowhere to put the number, and every body it made was unreachable the
        // moment it left the screen.
        //
        // Both directions for the same reason. The interaction is walking seed
        // space with what you have already found pinned, and a walk that only
        // goes forward makes the body you just passed unrecoverable.
        if ui
            .button("◀")
            .on_hover_text("the seed before this")
            .clicked()
        {
            editor.record.reroll(editor.record.seed.wrapping_sub(1));
            changed = true;
        }
        if ui
            .button("▶")
            .on_hover_text("the seed after this")
            .clicked()
        {
            editor.record.reroll(editor.record.seed.wrapping_add(1));
            changed = true;
        }
    });

    ui.horizontal_wrapped(|ui| {
        ui.label("locked");
        for category in Category::ALL {
            let mut locked = editor.record.locks.is_locked(category);
            if ui
                .toggle_value(&mut locked, category_name(category))
                .changed()
            {
                // A lock changes nothing about the body until the next
                // re-roll, so it is not a reason to rebuild one.
                editor.record.locks.toggle(category);
                noted = true;
            }
        }
    });
    if editor.record.locks.is_everything() {
        ui.small("every category locked: a re-roll would do nothing");
    } else {
        // What the hunt is actually searching, said plainly. Locking IS the
        // technique — keep what you have found, step the seed, judge only what
        // is still moving — and it stays invisible unless the panel says which
        // part of the body the next step is allowed to touch.
        let held = editor.record.locks.locked();
        if !held.is_empty() {
            let names: Vec<&str> = held.into_iter().map(category_name).collect();
            ui.small(format!("hunting, holding {}", names.join(", ")));
        }
    }
    (changed, noted)
}

/// The name to show a category by.
///
/// Eight since symbios-avatar #53 split the old `features` bit, which had come
/// to mean head shape, complexion, hair and hand size all at once. The three
/// that came out of it are the ones a creator most often wants to hold apart —
/// a face kept while its colouring is rolled — so they read as their own
/// toggles here rather than as one.
fn category_name(category: Category) -> &'static str {
    match category {
        Category::Stature => "stature",
        Category::Build => "build",
        Category::Frame => "frame",
        Category::Proportions => "proportions",
        Category::Head => "head",
        Category::Colouring => "colouring",
        Category::Hair => "hair",
        Category::Age => "age",
    }
}

/// The high tier: what the body is, said at the level a person is described at.
///
/// Four axes that each reach many quantities, against the dozens below that
/// each reach one. They sit at the top and open by default because that is the
/// order somebody should meet them in — decide who this is, then correct what
/// the formulas got wrong about them.
///
/// **Two of these are not exploration axes and must not get an envelope
/// slider.** `femininity` and `mass` are shape axes and carry the usual widened
/// range; `bodyFat` is a real fraction of body mass over its own bounds and
/// `age` is a count of whole years. Handing either the ±3 treatment would offer
/// a negative body-fat fraction and a two-hundred-year-old, which is why the
/// engine does not stretch them (symbios-avatar #162).
fn composite_axes(ui: &mut egui::Ui, record: &mut AvatarRecord) -> bool {
    use symbios_avatar::plan::{AGE_RANGE, BODY_FAT_RANGE};

    let mut changed = false;
    egui::CollapsingHeader::new("composites")
        .default_open(true)
        .show(ui, |ui| {
            let composites = &mut record.composites;
            changed |= explored(
                ui,
                "femininity",
                &mut composites.femininity,
                0.0,
                (-1.0, 1.0),
            );
            changed |= explored(ui, "mass", &mut composites.mass, 0.0, (-1.0, 1.0));
            changed |= axis(
                ui,
                "body fat",
                &mut composites.body_fat,
                BODY_FAT_RANGE.0..=BODY_FAT_RANGE.1,
            );
            // Whole years, so a slider that steps in thousandths would be
            // showing a record no reader can hold — the same argument the hair
            // lock count is on a plain integer slider for.
            changed |= ui
                .add(
                    egui::Slider::new(&mut composites.age, AGE_RANGE.0..=AGE_RANGE.1)
                        .text("age")
                        .suffix(" yr"),
                )
                .changed();
        });
    changed
}

/// The archetype's own axes, whichever archetype it is.
fn body_axes(ui: &mut egui::Ui, archetype: &mut Archetype) -> bool {
    let mut changed = false;
    egui::CollapsingHeader::new("body")
        .default_open(true)
        .show(ui, |ui| match archetype {
            Archetype::Humanoid(params) => {
                // Envelope ranges (symbios-avatar #160), and the head's two
                // skull axes join the panel: an exploration editor that
                // cannot reach `head_breadth` is missing the loudest axis a
                // skull has.
                let stature = symbios_avatar::HumanoidParams::height_envelope();
                changed |= axis(ui, "height", &mut params.height, stature.0..=stature.1);
                // `build` and `muscle` used to sit here and are gone
                // (symbios-avatar #164): they retired into the `mass` and
                // `bodyFat` composites above, which reach the same radii
                // allometrically rather than by one factor on all of them. The
                // panel keeps them in the composites section and not here,
                // which is the whole point of the two tiers — this block is the
                // per-region OFFSETS, and how heavy a body is was never one.
                changed |= explored(
                    ui,
                    "shoulder width",
                    &mut params.shoulder_width,
                    0.0,
                    (-1.0, 1.0),
                );
                changed |= explored(ui, "hip width", &mut params.hip_width, 0.0, (-1.0, 1.0));
                changed |= explored(ui, "limb length", &mut params.limb_length, 0.0, (-1.0, 1.0));
                changed |= explored(ui, "neck length", &mut params.neck_length, 0.0, (-1.0, 1.0));
                changed |= explored(ui, "head size", &mut params.head_size, 0.0, (-1.0, 1.0));
                changed |= explored(
                    ui,
                    "head breadth",
                    &mut params.head_breadth,
                    0.0,
                    (-1.0, 1.0),
                );
                changed |= explored(ui, "face length", &mut params.face_length, 0.0, (-1.0, 1.0));
                changed |= explored(
                    ui,
                    "extremity size",
                    &mut params.extremity_size,
                    0.0,
                    (-1.0, 1.0),
                );
            }
            Archetype::Quadruped(params) => {
                let stature = symbios_avatar::QuadrupedParams::height_envelope();
                changed |= axis(ui, "height", &mut params.height, stature.0..=stature.1);
                changed |= explored(ui, "body length", &mut params.body_length, 0.0, (-1.0, 1.0));
                changed |= explored(ui, "build", &mut params.build, 0.0, (-1.0, 1.0));
                changed |= explored(ui, "muscle", &mut params.muscle, 0.0, (0.0, 1.0));
                changed |= explored(ui, "leg length", &mut params.leg_length, 0.0, (-1.0, 1.0));
                changed |= explored(ui, "neck length", &mut params.neck_length, 0.0, (-1.0, 1.0));
                changed |= explored(ui, "head size", &mut params.head_size, 0.0, (-1.0, 1.0));
                changed |= explored(ui, "tail length", &mut params.tail_length, 0.0, (-1.0, 1.0));
            }
            // An archetype this build does not know about is kept verbatim
            // rather than edited into something else — a panel that offered
            // sliders for it would write a body over one it cannot read.
            Archetype::Unknown { type_name, .. } => {
                ui.small(format!("{type_name} is not an archetype this build knows"));
            }
        });
    changed
}

/// One complexion swatch's colour, as egui wants it.
///
/// `#[allow]`ed rather than worked around: the input is clamped to `0..=1` and
/// multiplied by 255 before rounding, so there is no truncation to lose and no
/// sign to lose either. Written once, here, so the exception is one line rather
/// than three at each channel.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a clamped unit float scaled to 0..=255 fits a byte by construction"
)]
fn swatch(tone: symbios_avatar::Vec3) -> egui::Color32 {
    let byte = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
    egui::Color32::from_rgb(byte(tone.x), byte(tone.y), byte(tone.z))
}

/// Complexion.
fn skin_axes(ui: &mut egui::Ui, record: &mut AvatarRecord) -> bool {
    let mut changed = false;
    egui::CollapsingHeader::new("skin").show(ui, |ui| {
        // **The ramp the engine already has, drawn** (#122). A melanin slider
        // is a number between two ends nobody can see, and a complexion is the
        // one axis on this record somebody arrives with an opinion about
        // already. The stops are sampled from `SkinParams::base_tone`, so these
        // are the engine's own curve rather than a palette invented here — a
        // swatch cannot drift from the tone it paints.
        //
        // They follow `undertone` too, which is the point of sampling rather
        // than hard-coding: the row restates itself as that slider moves, so
        // the interaction between the two axes is visible instead of guessed.
        ui.horizontal_wrapped(|ui| {
            for stop in 0..=10u32 {
                let melanin = f32::from(u16::try_from(stop).unwrap_or(0)) / 10.0;
                let probe = symbios_avatar::SkinParams {
                    melanin,
                    ..record.skin
                };
                let picked = (record.skin.melanin - melanin).abs() < 0.05;
                let button = egui::Button::new(if picked { "•" } else { " " })
                    .fill(swatch(probe.base_tone()))
                    .min_size(egui::vec2(20.0, 20.0));
                if ui
                    .add(button)
                    .on_hover_text(format!("{melanin:.1}"))
                    .clicked()
                {
                    record.skin.melanin = melanin;
                    changed = true;
                }
            }
        });
        changed |= axis(ui, "melanin", &mut record.skin.melanin, 0.0..=1.0);
        changed |= signed(ui, "undertone", &mut record.skin.undertone);
        changed |= axis(ui, "blush", &mut record.skin.blush, 0.0..=1.0);
        changed |= axis(ui, "freckles", &mut record.skin.freckles, 0.0..=1.0);
        changed |= axis(ui, "stubble", &mut record.skin.stubble, 0.0..=1.0);
    });
    changed
}

/// How the eyes are shaped and set.
fn eye_axes(ui: &mut egui::Ui, record: &mut AvatarRecord) -> bool {
    let mut changed = false;
    egui::CollapsingHeader::new("eyes").show(ui, |ui| {
        changed |= explored(ui, "size", &mut record.eyes.size, 0.5, (0.0, 1.0));
        changed |= explored(ui, "spacing", &mut record.eyes.spacing, 0.0, (-1.0, 1.0));
        changed |= explored(ui, "depth", &mut record.eyes.depth, 0.0, (-1.0, 1.0));
        changed |= explored(ui, "aperture", &mut record.eyes.aperture, 0.8, (0.0, 1.0));
    });
    changed
}

/// Nose, brow, mouth and ears.
fn face_axes(ui: &mut egui::Ui, record: &mut AvatarRecord) -> bool {
    let mut changed = false;
    egui::CollapsingHeader::new("face").show(ui, |ui| {
        changed |= explored(ui, "nose", &mut record.face.nose, 0.5, (0.0, 1.0));
        changed |= explored(
            ui,
            "nose width",
            &mut record.face.nose_width,
            0.5,
            (0.0, 1.0),
        );
        changed |= explored(ui, "brow", &mut record.face.brow, 0.5, (0.0, 1.0));
        changed |= explored(ui, "mouth", &mut record.face.mouth, 0.5, (0.0, 1.0));
        changed |= explored(
            ui,
            "mouth width",
            &mut record.face.mouth_width,
            0.5,
            (0.0, 1.0),
        );
        changed |= explored(ui, "ears", &mut record.face.ears, 0.5, (0.0, 1.0));
    });
    changed
}

/// Hair.
fn hair_axes(ui: &mut egui::Ui, record: &mut AvatarRecord) -> bool {
    let mut changed = false;
    egui::CollapsingHeader::new("hair").show(ui, |ui| {
        changed |= axis(ui, "length", &mut record.hair.length, 0.0..=1.0);
        // Signed, not unit. Both of these clamp to `-1..=1` in the engine and
        // shipped here on a `0..=1` slider, so the whole negative half of each
        // — thin hair and a receding hairline — was unreachable from the panel
        // for as long as it existed (#9). Nothing failed: the values were legal
        // and the body built, the axis simply had no way to be asked for.
        changed |= signed(ui, "volume", &mut record.hair.volume);
        changed |= signed(ui, "coverage", &mut record.hair.coverage);
        changed |= signed(ui, "part", &mut record.hair.part);
        changed |= axis(ui, "wave", &mut record.hair.wave, 0.0..=1.0);
        changed |= axis(ui, "shade", &mut record.hair.shade, 0.0..=1.0);
        changed |= axis(ui, "curl", &mut record.hair.curl, 0.0..=1.0);
        // A count, not an axis: the record stores it as a whole number and a
        // slider showing 11.5 locks would be showing a body nobody can have.
        changed |= ui
            .add(
                egui::Slider::new(
                    &mut record.hair.locks,
                    symbios_avatar::hair::MIN_LOCKS..=symbios_avatar::hair::MAX_LOCKS,
                )
                .text("locks"),
            )
            .changed();
    });
    changed
}

/// What the body is wearing.
fn outfit_axes(ui: &mut egui::Ui, record: &mut AvatarRecord) -> bool {
    let mut changed = false;
    egui::CollapsingHeader::new("outfit").show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label("sleeve");
            for cut in [Sleeve::Bare, Sleeve::Forearm, Sleeve::Wrist] {
                let picked = record.outfit.sleeve == cut;
                if ui.selectable_label(picked, sleeve_name(&cut)).clicked() && !picked {
                    record.outfit.sleeve = cut.clone();
                    changed = true;
                }
            }
        });
        ui.horizontal(|ui| {
            ui.label("leg");
            for cut in [Leg::Shorts, Leg::Calf, Leg::Ankle] {
                let picked = record.outfit.leg == cut;
                if ui.selectable_label(picked, leg_name(&cut)).clicked() && !picked {
                    record.outfit.leg = cut.clone();
                    changed = true;
                }
            }
        });
        changed |= axis(ui, "top hue", &mut record.outfit.top_hue, 0.0..=1.0);
        changed |= axis(ui, "top shade", &mut record.outfit.top_shade, 0.0..=1.0);
        changed |= axis(ui, "leg hue", &mut record.outfit.leg_hue, 0.0..=1.0);
        changed |= axis(ui, "leg shade", &mut record.outfit.leg_shade, 0.0..=1.0);
    });
    changed
}

/// The name to show a sleeve cut by.
fn sleeve_name(cut: &Sleeve) -> String {
    match cut {
        Sleeve::Bare => String::from("bare"),
        Sleeve::Forearm => String::from("forearm"),
        Sleeve::Wrist => String::from("wrist"),
        Sleeve::Other(other) => other.clone(),
    }
}

/// The name to show a trouser cut by.
fn leg_name(cut: &Leg) -> String {
    match cut {
        Leg::Shorts => String::from("shorts"),
        Leg::Calf => String::from("calf"),
        Leg::Ankle => String::from("ankle"),
        Leg::Other(other) => other.clone(),
    }
}

/// A `0..=1`-ish axis on a slider that steps in thousandths.
///
/// A thousandth is exactly what the wire format carries, so the slider cannot
/// land between two values a record can hold. What it shows is what would be
/// written.
fn axis(
    ui: &mut egui::Ui,
    name: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
) -> bool {
    ui.add(
        egui::Slider::new(value, range)
            .text(name)
            .step_by(0.001)
            .fixed_decimals(3),
    )
    .changed()
}

/// A `-1..=1` axis.
fn signed(ui: &mut egui::Ui, name: &str, value: &mut f32) -> bool {
    axis(ui, name, value, -1.0..=1.0)
}

/// A shape axis over its exploration envelope (symbios-avatar #160).
///
/// The range comes from the engine's own [`explore_range`] over the axis's
/// default and conservative span, so the sliders and `sanitize` cannot
/// disagree about where an axis ends. Style axes — complexion, hair, outfit —
/// deliberately keep their classic sliders; the envelope is a shape idea.
///
/// [`explore_range`]: symbios_avatar::plan::explore_range
fn explored(
    ui: &mut egui::Ui,
    name: &str,
    value: &mut f32,
    default: f32,
    conservative: (f32, f32),
) -> bool {
    let (low, high) = symbios_avatar::plan::explore_range(default, conservative);
    axis(ui, name, value, low..=high)
}

/// What the plan actually produced, so the low tier is not edited blind.
///
/// An offset is a correction to a number somebody cannot see, which is a hard
/// thing to aim: "shoulder width +0.3" says nothing about how wide the
/// shoulders ended up. This prints the quantities the derivation hands the
/// cage, refreshed from the record on every draw.
///
/// **Two labels here are doing real work, and both name traps this crate has
/// fallen into more than once.** The radii are the CAGE's, and subdivision
/// pulls the rendered surface inside them — comparing one of these to a
/// measured body is the error #106 spent a session on, where a shoulder was
/// pushed out to clear a ribcage half again wider than the visible one. And the
/// fractions are of NOMINAL stature, not of the built body's height, because
/// a fraction of rendered height silently changes whenever anything moves the
/// head — which is how every band figure on #106 went stale without a
/// coefficient being touched.
///
/// Costs nothing while the header is shut: egui only runs the body of an open
/// one, and the skeleton is rebuilt inside it rather than cached, so what is
/// shown cannot lag what is set.
fn derived(ui: &mut egui::Ui, record: &AvatarRecord) {
    egui::CollapsingHeader::new("derived").show(ui, |ui| {
        let stature = match &record.archetype {
            Archetype::Humanoid(params) => params.height,
            Archetype::Quadruped(params) => params.height,
            Archetype::Unknown { .. } => {
                ui.small("nothing to derive from a body this build cannot read");
                return;
            }
        };
        let skeleton = record.skeleton();

        ui.small(format!("stature      {stature:.3} m nominal"));
        for (name, zone) in [
            ("pelvis", Zone::Pelvis),
            ("waist", Zone::Abdomen),
            ("chest", Zone::Chest),
            ("neck", Zone::Neck),
            ("head", Zone::Head),
        ] {
            // The first node of a zone, which is the same rule the engine's own
            // tests select by — a zone can hold several and picking a different
            // one would make this readout disagree with them.
            let Some(node) = skeleton.nodes.iter().find(|node| node.zone == zone) else {
                continue;
            };
            ui.small(format!(
                "{name:<8} r   {:5.1} cm   {:.4} of stature",
                node.radius * 100.0,
                node.radius / stature
            ));
        }

        // Spans off the root of each limb chain, never the widest joint of a
        // zone: on an A-posed arm the widest joint is the ELBOW, which is the
        // measurement error `the_default_body_stands_near_the_proportion_canon`
        // carries a paragraph about.
        if let Ok(rig) = Rig::from_skeleton(&skeleton) {
            for (name, limb) in [("shoulder", Limb::ForeLeft), ("hip", Limb::HindLeft)] {
                let Some(chain) = rig.limb_chain(limb) else {
                    continue;
                };
                let span = rig.joints[chain[0]].position.x.abs() * 2.0;
                ui.small(format!(
                    "{name:<8} span{:5.1} cm   {:.4} of stature",
                    span * 100.0,
                    span / stature
                ));
            }
        }

        ui.small("cage radii — the rendered surface sits inside these");
        ui.small("fractions are of nominal stature, not of rendered height");
    });
}

/// The record as JSON, out and back in.
fn wire(ui: &mut egui::Ui, editor: &mut RecordEditor) {
    egui::CollapsingHeader::new("record").show(ui, |ui| {
        ui.horizontal(|ui| {
            if ui.button("copy").clicked() {
                ui.ctx().copy_text(editor.json.clone());
                editor.status = format!("copied {} bytes", editor.json.len());
            }
            if ui.button("load").clicked() {
                load(editor);
            }
            if ui.button("discard edits").clicked() {
                editor.json = to_json(&editor.record);
                editor.json_state = Json::Following;
                editor.status.clear();
            }
        });
        let box_changed = ui
            .add(
                egui::TextEdit::multiline(&mut editor.json)
                    .code_editor()
                    .desired_rows(8)
                    .desired_width(f32::INFINITY),
            )
            .changed();
        if box_changed {
            // Once somebody has typed in here it is theirs. Refreshing it from
            // the record on the next edit would silently eat a pasted record,
            // which looks exactly like a load that does not work.
            editor.json_state = Json::Edited;
        }
        let size = editor.record.serialized_size().unwrap_or(0);
        ui.small(format!(
            "{size} bytes of {}",
            symbios_avatar::record::RECORD_BUDGET_BYTES
        ));
        if !editor.status.is_empty() {
            ui.small(editor.status.clone());
        }
    });
}

/// The look as a share code, out and back in.
///
/// A code carries a *look* and not a record — archetype, composites and
/// complexion, each axis quantised to a byte — so importing one keeps the name,
/// the seed and the locks of the record it lands in. That is the point of it:
/// a code is for passing a face between people, not for moving an avatar.
///
/// Deliberately **lossy and said so on screen**. Re-encoding a code is not a
/// round trip through the record, and somebody who imports a code and sees an
/// axis read 0.247 where their friend's read 0.250 should be able to find out
/// why without reading the source.
fn share(ui: &mut egui::Ui, editor: &mut RecordEditor) {
    egui::CollapsingHeader::new("share code").show(ui, |ui| {
        ui.horizontal(|ui| {
            if ui.button("copy").clicked() {
                ui.ctx().copy_text(editor.code.clone());
                editor.status = format!("copied {}", editor.code);
            }
            if ui.button("import").clicked() {
                import(editor);
            }
            if ui.button("discard edits").clicked() {
                editor.code = editor.record.share_code();
                editor.code_state = Json::Following;
                editor.status.clear();
            }
        });
        let box_changed = ui
            .add(
                egui::TextEdit::singleline(&mut editor.code)
                    .code_editor()
                    .desired_width(f32::INFINITY),
            )
            .changed();
        if box_changed {
            // Once somebody has pasted here it is theirs, exactly as the JSON
            // box works — refreshing it from the record on the next edit is
            // indistinguishable from an import that silently does nothing.
            editor.code_state = Json::Edited;
        }
        ui.small("a look, not a record: the name, seed and locks stay");
        ui.small("one byte an axis, so a code is lossy by design");
    });
}

/// Parses the share-code box into the record's look.
///
/// Applied to a copy and swapped in only once it has parsed, so a mistyped code
/// cannot leave a body half-replaced — `apply_share_code` writes the archetype
/// before it can discover the complexion is short.
fn import(editor: &mut RecordEditor) {
    let mut candidate = editor.record.clone();
    match candidate.apply_share_code(&editor.code) {
        Ok(()) => {
            editor.record = candidate;
            editor.code = editor.record.share_code();
            editor.code_state = Json::Following;
            editor.status = String::from("imported");
            editor.touched();
        }
        Err(error) => editor.status = format!("not a share code: {error}"),
    }
}

/// Parses the JSON box into the record.
fn load(editor: &mut RecordEditor) {
    match serde_json::from_str::<AvatarRecord>(&editor.json) {
        Ok(mut parsed) => {
            // Sanitised on the way in, exactly as a record off the network is:
            // the panel has no more right to trust a pasted record than a PDS
            // has.
            parsed.sanitize();
            editor.record = parsed;
            editor.json = to_json(&editor.record);
            editor.json_state = Json::Following;
            editor.status = String::from("loaded");
            editor.dirty = true;
            editor.still = Duration::ZERO;
        }
        Err(error) => editor.status = format!("not a record: {error}"),
    }
}

/// The record as the JSON a PDS would hold, indented to be read.
fn to_json(record: &AvatarRecord) -> String {
    serde_json::to_string_pretty(record).unwrap_or_else(|error| format!("// {error}"))
}

/// What the body costs, how long it took, and the one measurement worth a
/// button.
fn readout(ui: &mut egui::Ui, editor: &mut RecordEditor, bodies: &Query<&AvatarBody>) {
    if let Some(body) = bodies.iter().next() {
        let budget = body.avatar.budget;
        ui.small(format!(
            "{} tris / 30000 · {} draws · {} KiB texture",
            budget.tris,
            budget.meshes,
            budget.texture_bytes / 1024
        ));
    }
    if let Some((took, atlas)) = editor.last_build {
        let ms = took.as_secs_f32() * 1000.0;
        ui.small(format!(
            "rebuilt in {ms:.0} ms at atlas {atlas}{}",
            if editor.draft { " (draft)" } else { "" }
        ));
    }
    ui.small(format!(
        "panel {:.2} ms",
        editor.last_panel.as_secs_f32() * 1000.0
    ));

    ui.horizontal(|ui| {
        if ui.button("measure aperture").clicked() {
            editor.measuring = Measuring::Asked;
        }
        if editor.measuring == Measuring::Asked {
            ui.label("measuring, about two seconds…");
        }
    });
    if let Measuring::Done(lines) = &editor.measuring {
        for line in lines {
            ui.small(line);
        }
    }
}

/// The bare eye, three ways, each naming an owner.
///
/// Skin and lids is what a viewer sees; skin alone says what the face owns;
/// lids alone says what the lids own. Reading them together is what diagnosed
/// an eye whose lateral edge was owned by nothing and ran 97 degrees round the
/// side of the head.
///
/// About two seconds in release and half a minute in debug — a containment test
/// per sample per occluder — which is why it is a button and not a readout.
#[must_use]
pub fn measure_aperture(body: Option<&AvatarBody>) -> Vec<String> {
    let Some(body) = body else {
        return vec![String::from("no body to measure")];
    };
    let Some(eyes) = &body.avatar.parts.eyes else {
        return vec![String::from("this body has no eyes")];
    };
    let head = body.avatar.rig.joints[eyes.head].position;
    let skin = Some((&body.avatar.parts.body, head));
    let mut lines = vec![String::from("left eye        share   centre az   spans az")];
    for (what, aperture) in [
        ("skin and lids", eyes.left.aperture(skin, true)),
        ("skin alone", eyes.left.aperture(skin, false)),
        ("lids alone", eyes.left.aperture(None, true)),
    ] {
        lines.push(format!(
            "{what:<14} {:5.1}%     {:+6.1}   {:+6.1}..{:+.1}",
            aperture.share * 100.0,
            aperture.centre.0.to_degrees(),
            aperture.span.0.to_degrees(),
            aperture.span.1.to_degrees(),
        ));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use symbios_avatar::LockSet;

    /// Every axis the panel writes, as a path and the value at it.
    ///
    /// Named rather than compared as a whole so a failure says which axis, and
    /// so adding an axis to the panel without adding it here is visible.
    fn axes(record: &AvatarRecord) -> Vec<(&'static str, f32)> {
        let mut out = match &record.archetype {
            Archetype::Humanoid(p) => vec![
                ("height", p.height),
                ("shoulder_width", p.shoulder_width),
                ("hip_width", p.hip_width),
                ("limb_length", p.limb_length),
                ("neck_length", p.neck_length),
                ("head_size", p.head_size),
                // These two were edited by the panel and covered by nothing for
                // as long as they existed (#9): a slider bound to the wrong
                // field of the pair would have passed this suite.
                ("head_breadth", p.head_breadth),
                ("face_length", p.face_length),
                ("extremity_size", p.extremity_size),
            ],
            Archetype::Quadruped(p) => vec![
                ("height", p.height),
                ("body_length", p.body_length),
                ("build", p.build),
                ("muscle", p.muscle),
                ("leg_length", p.leg_length),
                ("neck_length", p.neck_length),
                ("head_size", p.head_size),
                ("tail_length", p.tail_length),
            ],
            Archetype::Unknown { .. } => Vec::new(),
        };
        out.extend([
            // The high tier (symbios-avatar #162). `age` is not here: it is a
            // count of years, and `counts` covers it.
            ("femininity", record.composites.femininity),
            ("mass", record.composites.mass),
            ("body_fat", record.composites.body_fat),
            ("melanin", record.skin.melanin),
            ("undertone", record.skin.undertone),
            ("blush", record.skin.blush),
            ("freckles", record.skin.freckles),
            ("stubble", record.skin.stubble),
            ("eye size", record.eyes.size),
            ("eye spacing", record.eyes.spacing),
            ("eye depth", record.eyes.depth),
            ("eye aperture", record.eyes.aperture),
            ("nose", record.face.nose),
            ("nose width", record.face.nose_width),
            ("brow", record.face.brow),
            ("mouth", record.face.mouth),
            ("mouth width", record.face.mouth_width),
            ("ears", record.face.ears),
            ("hair length", record.hair.length),
            ("hair volume", record.hair.volume),
            ("hair coverage", record.hair.coverage),
            ("hair part", record.hair.part),
            ("hair wave", record.hair.wave),
            ("hair shade", record.hair.shade),
            ("hair curl", record.hair.curl),
            ("top hue", record.outfit.top_hue),
            ("top shade", record.outfit.top_shade),
            ("leg hue", record.outfit.leg_hue),
            ("leg shade", record.outfit.leg_shade),
        ]);
        out
    }

    /// Every whole-number control the panel writes.
    ///
    /// Kept apart from [`axes`] rather than cast into it, because a count is a
    /// different kind of thing: it has no thousandth to land on, and the
    /// question asked of it is whether it survives the wire as the integer it
    /// is. Casting them to `f32` to share one list is what the panel is for
    /// NOT doing — a slider showing 11.5 locks or 40.5 years shows a record
    /// nobody can hold.
    fn counts(record: &AvatarRecord) -> Vec<(&'static str, u32)> {
        vec![
            ("age", record.composites.age),
            ("hair locks", record.hair.locks),
        ]
    }

    /// A record with every axis dragged somewhere a slider can put it and
    /// nowhere a thousandth lands.
    fn fiddled() -> AvatarRecord {
        let mut record = AvatarRecord::new("Fiddled", Archetype::default());
        if let Archetype::Humanoid(params) = &mut record.archetype {
            params.height = 1.812_34;
            params.shoulder_width = 0.876_54;
            params.hip_width = -0.098_76;
            params.limb_length = 0.345_67;
            params.neck_length = -0.765_43;
            params.head_size = 0.111_11;
            params.head_breadth = 0.654_32;
            params.face_length = -0.543_21;
            params.extremity_size = -0.999_99;
        }
        record.composites.femininity = 0.371_53;
        record.composites.mass = -0.628_47;
        record.composites.body_fat = 0.317_29;
        record.composites.age = 53;
        record.skin.melanin = 0.456_78;
        record.skin.undertone = -0.333_33;
        record.skin.blush = 0.777_77;
        record.skin.freckles = 0.123_45;
        record.skin.stubble = 0.987_65;
        record.eyes.size = 0.543_21;
        record.eyes.spacing = -0.246_81;
        record.eyes.depth = 0.135_79;
        record.eyes.aperture = 0.864_20;
        record.face.nose = 0.192_83;
        record.face.nose_width = 0.418_27;
        record.face.brow = 0.746_51;
        record.face.mouth = 0.303_03;
        record.face.mouth_width = 0.572_91;
        record.face.ears = 0.606_06;
        record.hair.length = 0.717_17;
        // Negative, because the half of these two axes the panel could not
        // reach is exactly the half a test that never set them could not catch.
        record.hair.volume = -0.282_82;
        record.hair.coverage = -0.454_54;
        record.hair.part = -0.616_16;
        record.hair.wave = 0.838_38;
        record.hair.shade = 0.070_70;
        record.hair.curl = 0.929_29;
        record.hair.locks = 17;
        record.outfit.top_hue = 0.151_51;
        record.outfit.top_shade = 0.626_26;
        record.outfit.leg_hue = 0.373_73;
        record.outfit.leg_shade = 0.848_48;
        record
    }

    #[test]
    fn every_axis_the_panel_writes_survives_the_wire_exactly() {
        // The whole reason the panel sanitises on every edit rather than
        // showing the raw slider value. The wire format is scaled integers in
        // thousandths and has no floats at all, so a panel reading 0.456_78 is
        // showing a number no record can hold — and the body somebody rebuilds
        // from the emitted record is not the body that was judged.
        //
        // Bit-exact, not near: a tolerance here would hide exactly the drift
        // this is looking for.
        let mut editor = RecordEditor::new(fiddled());
        editor.touched();

        let json = to_json(&editor.record);
        let mut back: AvatarRecord = serde_json::from_str(&json).expect("its own JSON parses");
        back.sanitize();

        for ((name, before), (_, after)) in axes(&editor.record).iter().zip(axes(&back)) {
            assert_eq!(
                before.to_bits(),
                after.to_bits(),
                "{name} went out as {before} and came back as {after}"
            );
        }
        for ((name, before), (_, after)) in counts(&editor.record).iter().zip(counts(&back)) {
            assert_eq!(
                *before, after,
                "{name} went out as {before} and came back as {after}"
            );
        }
        assert_eq!(
            editor.record, back,
            "the record did not survive its own JSON"
        );
    }

    #[test]
    fn the_coverage_list_names_every_axis_a_record_carries() {
        // A ratchet on the list above, not on the panel. What these tests can
        // check is that every axis survives the wire and lands on a thousandth;
        // what they cannot check is that a slider is bound to the field its
        // label names, because nothing here drives the UI. So the failure mode
        // they DO have is an axis quietly missing from the list — which is how
        // `head_breadth`, `face_length`, `nose_width` and `mouth_width` went
        // four axes uncovered from the day they were added (#9).
        //
        // Counting is the cheapest guard that survives the next addition: add a
        // field to the record, and this fails until somebody has decided
        // whether the panel writes it.
        //
        // **40 → 38** (symbios-avatar #164): `build` and `muscle` retired into
        // the `mass` and `bodyFat` composites, which the panel already carries.
        // This is the guard doing its job in the removal direction — the count
        // fell and somebody had to decide the two sliders were gone rather than
        // missing.
        let record = fiddled();
        let listed = axes(&record).len();
        assert_eq!(
            listed, 38,
            "the panel's coverage list names {listed} axes; if a record field \
             was added or removed, add it to `axes` and `fiddled` and correct \
             this count"
        );
        assert_eq!(counts(&record).len(), 2, "and the same for whole numbers");
    }

    #[test]
    fn a_slider_never_shows_a_value_the_wire_cannot_carry() {
        // The other half: sanitising has to reach every axis the panel writes.
        // An axis added to the panel and forgotten in some sanitize() would
        // pass the round trip above only if serialisation happened to round the
        // same way, so this asserts the value is *on* a thousandth.
        let mut editor = RecordEditor::new(fiddled());
        editor.touched();
        for (name, value) in axes(&editor.record) {
            let thousandths = value * 1000.0;
            assert!(
                (thousandths - thousandths.round()).abs() < 1e-3,
                "{name} shows {value}, which is not a thousandth"
            );
        }
    }

    #[test]
    fn a_share_code_moves_a_look_and_leaves_the_identity_alone() {
        // What a code is for: passing a face between people. The name, the seed
        // and the locks belong to the record it lands in, not to the code.
        let source = RecordEditor::new(fiddled());
        let code = source.code.clone();

        let mut target = RecordEditor::new(AvatarRecord::new("Mine", Archetype::default()));
        target.record.seed = 4321;
        target.record.locks = LockSet::NONE.with(Category::Hair);
        target.code = code;
        import(&mut target);

        assert_eq!(target.status, "imported");
        assert_eq!(target.record.name, "Mine", "the name stayed");
        assert_eq!(target.record.seed, 4321, "and the seed");
        assert_eq!(target.record.locks, LockSet::NONE.with(Category::Hair));

        // The look travelled. Codes are one byte an axis, so this is a
        // tolerance and the engine's own tests say the same.
        let (Archetype::Humanoid(from), Archetype::Humanoid(to)) =
            (&source.record.archetype, &target.record.archetype)
        else {
            panic!("archetype changed");
        };
        assert!((from.height - to.height).abs() < 0.002);
        assert!(
            (source.record.composites.femininity - target.record.composites.femininity).abs()
                < 0.03
        );
        assert!((source.record.skin.melanin - target.record.skin.melanin).abs() < 0.01);

        // And the box went back to following, so it now shows the code for the
        // body actually on screen rather than the one that was pasted.
        assert_eq!(target.code_state, Json::Following);
        assert_eq!(target.code, target.record.share_code());
    }

    #[test]
    fn a_mistyped_share_code_changes_nothing_at_all() {
        // `apply_share_code` writes the archetype before it can discover the
        // payload is short, so importing in place would leave a body half
        // replaced. This is why `import` works on a copy.
        let mut editor = RecordEditor::new(fiddled());
        let before = editor.record.clone();
        editor.code = String::from("PPPPP-PPPPP-PPPPP");
        import(&mut editor);

        assert!(
            editor.status.starts_with("not a share code"),
            "status was {}",
            editor.status
        );
        assert_eq!(editor.record, before, "a refused code moved something");
    }

    #[test]
    fn the_share_code_box_stops_following_once_it_is_typed_in() {
        let mut editor = RecordEditor::new(fiddled());
        editor.code_state = Json::Edited;
        editor.code = String::from("someone else's code");
        editor.record.name = String::from("Renamed");
        editor.restate();
        assert_eq!(
            editor.code, "someone else's code",
            "restating ate a pasted code"
        );
    }

    #[test]
    fn loading_a_record_replaces_the_one_being_edited() {
        let mut editor = RecordEditor::new(AvatarRecord::new("Before", Archetype::default()));
        let other = fiddled().named("After");
        editor.json = to_json(&other);
        load(&mut editor);

        assert_eq!(editor.record.name, "After");
        assert!(editor.dirty, "a loaded record did not ask for a rebuild");
        assert_eq!(editor.json_state, Json::Following);
        let mut sanitised = other;
        sanitised.sanitize();
        assert_eq!(editor.record, sanitised);
    }

    #[test]
    fn a_record_that_is_not_a_record_says_so_and_changes_nothing() {
        let mut editor = RecordEditor::new(AvatarRecord::new("Kept", Archetype::default()));
        editor.json = String::from("{ this is not JSON");
        load(&mut editor);
        assert_eq!(editor.record.name, "Kept");
        assert!(editor.status.starts_with("not a record"));
    }

    #[test]
    fn the_json_box_stops_following_once_it_is_typed_in() {
        // Refreshing a box somebody has pasted into is how a working load looks
        // broken: the paste vanishes on the next slider drag.
        let mut editor = RecordEditor::new(AvatarRecord::new("Viewed", Archetype::default()));
        editor.json_state = Json::Edited;
        let pasted = editor.json.clone();
        editor.record.face.nose = 0.9;
        editor.touched();
        assert_eq!(editor.json, pasted, "an edited box was overwritten");
    }

    #[test]
    fn a_re_roll_leaves_locked_categories_alone() {
        let mut editor = RecordEditor::new(AvatarRecord::new("Viewed", Archetype::default()));
        editor.record.locks = LockSet::NONE.with(Category::Stature);
        editor.reroll(11);
        let held = match editor.record.archetype {
            Archetype::Humanoid(params) => params.height,
            _ => unreachable!("a humanoid was asked for"),
        };
        editor.reroll(12);
        let still = match editor.record.archetype {
            Archetype::Humanoid(params) => params.height,
            _ => unreachable!("a humanoid was asked for"),
        };
        // Bit-exact on purpose. A lock is a promise that a value did not
        // change, and a tolerance would let it drift a thousandth a re-roll.
        assert_eq!(
            held.to_bits(),
            still.to_bits(),
            "a locked stature was re-rolled: {held} became {still}"
        );
    }
}
