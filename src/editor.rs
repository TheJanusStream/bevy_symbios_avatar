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
//! key, and it never draws at all under the viewer's `--shot`. What is
//! photographed is a body, a light and a camera, and nothing else.
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
//! twice, which is the engine's own problem to solve and not this crate's.
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
//!
//! ## Hosting the sections in another app
//!
//! The panel is composed from public per-section functions, and those — not
//! the panel — are the reuse surface. A host with its own window, theme, undo
//! and rebuild pipeline calls the sections against its own record and owes
//! this module nothing else: no [`RecordEditor`] resource, no
//! [`RecordEditorPlugin`], no [`EditedAvatar`] entity. Every section takes
//! `&mut egui::Ui` and writes one [`AvatarRecord`] (or its archetype), and
//! returns whether it changed something that changes the *body* — the host's
//! cue to sanitise and rebuild on whatever schedule it owns. That split is the
//! contract: **the sections write the record and say so; what a change costs
//! is the host's business.** This keeps one source of truth for axis widgets
//! across every host, which is the standing rule that each engine schema
//! change carries an editor slice — there is exactly one place a new axis has
//! to learn to draw itself.
//!
//! ```no_run
//! use bevy_egui::egui;
//! use bevy_symbios_avatar::editor;
//! use symbios_avatar::AvatarRecord;
//!
//! /// A host's own panel: same sections, its own chrome and rebuild.
//! fn body_sections(ui: &mut egui::Ui, record: &mut AvatarRecord) -> bool {
//!     let mut changed = editor::composite_axes(ui, record);
//!     changed |= editor::body_axes(ui, record);
//!     changed |= editor::skin_axes(ui, record);
//!     changed |= editor::coat_axes(ui, record);
//!     changed |= editor::fur_axes(ui, record);
//!     changed |= editor::eye_axes(ui, record);
//!     changed |= editor::face_axes(ui, record);
//!     changed |= editor::hair_axes(ui, record);
//!     changed |= editor::outfit_axes(ui, record);
//!     if changed {
//!         // Snaps every axis to the thousandth the wire format stores; the
//!         // host decides when the 68–277 ms rebuild is paid for.
//!         record.sanitize();
//!     }
//!     changed
//! }
//! ```

use std::time::Duration;

use bevy::mesh::skinning::SkinnedMeshInverseBindposes;
use bevy::platform::time::Instant;
use bevy::prelude::*;
use bevy_egui::{EguiContexts, EguiPrimaryContextPass, egui};
use symbios_avatar::dress::{self, GarmentParams, GarmentTexture, SurfaceConfig};
use symbios_avatar::plan::{Agouti, Fantasy, Pattern, Pigment, White};
use symbios_avatar::{
    Archetype, Avatar, AvatarConfig, AvatarRecord, BodyPlan, Category, Family, Feet,
    GENERATOR_VERSION, Horn, HumanoidParams, Limb, Nose, Pupil, QuadrupedParams, Rig, Zone,
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
#[expect(
    clippy::struct_excessive_bools,
    reason = "three of them are independent facts about one pipeline — an edit \
              outstanding, a build on the pool, a draft standing in — and all \
              four combinations of the first two occur; an enum would have to \
              pretend they exclude each other, and `open` is unrelated chrome"
)]
pub struct RecordEditor {
    /// The record. Every control writes here and nowhere else.
    pub record: AvatarRecord,
    /// Whether the panel draws at all.
    pub open: bool,
    /// Set when a control changed the record and the body has not caught up.
    dirty: bool,
    /// Whether the body on screen was built at [`DRAFT_ATLAS`].
    draft: bool,
    /// Whether a build is on the pool right now.
    ///
    /// Mirrors the system's own in-flight job, which lives in a `Local` and so
    /// cannot be seen from outside. Held here because [`Self::settled`] would
    /// otherwise lie for the whole length of a build: between the frame an edit
    /// spawns one and the frame it lands, `dirty` is already false and `draft`
    /// still describes the *previous* body.
    building: bool,
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
            building: false,
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

    /// Whether the body on screen is the finished one this record describes.
    ///
    /// **What a host must wait for before photographing anything.** A body
    /// arrives in stages — nothing at all, then a [`DRAFT_ATLAS`]
    /// stand-in, then the full build — and every stage before the last is a
    /// body somebody would draw the wrong conclusion from: the draft's
    /// complexion is not the complexion that ships, and an empty frame looks
    /// exactly like a record that failed to build. A capture fired on a frame
    /// count alone will photograph a scene with no hair, eyes or cloth in it.
    ///
    /// False while an edit is outstanding, while a build is on the pool, while
    /// a draft stands in, and before the first body has ever landed. Note that
    /// a record describing no body never settles — there is no body to settle
    /// on — so a caller that blocks on this needs a way out of the wait.
    #[must_use]
    pub fn settled(&self) -> bool {
        !self.dirty && !self.building && !self.draft && self.last_build.is_some()
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

/// A build in flight on the compute pool.
///
/// At most one exists at a time: a second build of a record that is still
/// moving would only be stale sooner. Edits made while it runs mark the editor
/// dirty again, and the *next* spawn takes the record as it is then — the
/// latest description always wins.
pub struct BuildJob {
    task: bevy::tasks::Task<Option<(Avatar, Duration)>>,
    atlas: u32,
    full: bool,
}

/// Rebuilds the edited body when the record has changed.
///
/// Two rebuilds, not one. A draft goes up as fast as builds complete so an
/// axis can be watched across its range; the full one lands once the axis has
/// been still for [`SETTLE`], because the complexion at [`DRAFT_ATLAS`] is not
/// the complexion that ships and a judgement made on it would be a judgement
/// about a texture nobody will see.
///
/// **The build itself runs on the compute pool, and the frame never waits for
/// it.** The panel writes the record, and the expensive consequence lands when
/// it lands: on the main thread a draft would be 68 ms of stall per edit and the
/// settle build 277 ms. The system spawns the build, polls, and swaps the body
/// in on the frame the task finishes. Timing is measured *inside* the task so
/// the readout reports the build, not the queue.
///
/// It builds and draws the body itself rather than asking [`SpawnAvatar`] for
/// one, so the timing instrument stays honest for the same reason it always
/// did.
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
    mut building: Local<Option<BuildJob>>,
) {
    use bevy::tasks::{AsyncComputeTaskPool, block_on, futures_lite::future};

    // Land a finished build first, so the swap happens the frame it is ready.
    let finished = building.as_mut().and_then(|job| {
        block_on(future::poll_once(&mut job.task)).map(|built| (built, job.atlas, job.full))
    });
    if let Some((built, atlas, full)) = finished {
        *building = None;
        editor.building = false;
        if let Some((avatar, took)) = built {
            for entity in &edited {
                // Despawned and rebuilt rather than patched, exactly as a
                // re-roll is: every mesh, chart and weight belongs to the
                // body that changed.
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
            editor.draft = !full;
            editor.last_build = Some((took, atlas));
        } else {
            // A record that describes no body is a record, not a crash — and
            // a panel is exactly where one gets made, by driving two limbs
            // into each other. Say so, keep the body that is up, and owe
            // nothing until the record changes again — a draft flag left
            // standing here would respawn the same doomed build every settle.
            editor.draft = false;
            editor.status =
                String::from("that record describes no body; showing the last one that did");
        }
    }

    // The settle clock runs whenever nothing is dirty, in-flight build or not:
    // stillness is a fact about the user's hands, not about the pool.
    if !editor.dirty {
        editor.still += time.delta();
    }
    if building.is_some() {
        return;
    }
    let full = if editor.dirty {
        false
    } else if editor.draft && editor.still >= SETTLE {
        // A draft is a stand-in, so the full build is owed even though nothing
        // has been touched since it went up.
        true
    } else {
        return;
    };

    let config = AvatarConfig {
        atlas: if full {
            AvatarConfig::default().atlas
        } else {
            DRAFT_ATLAS
        },
        // The far hair tier as well (#48), so the body on screen is the one a
        // peer sees from across a room when the camera is pulled back past
        // `HairLod::switch`.
        far_hair: true,
        ..AvatarConfig::default()
    };
    let atlas = config.atlas;
    let record = editor.record.clone();
    let task = AsyncComputeTaskPool::get().spawn(async move {
        let at = Instant::now();
        Avatar::build_with(&record, &config).map(|avatar| (avatar, at.elapsed()))
    });
    *building = Some(BuildJob { task, atlas, full });
    editor.building = true;
    editor.dirty = false;
    editor.still = Duration::ZERO;
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
    // egui 0.35 unified SidePanel/TopBottomPanel into `Panel`, and panels now
    // show into a `Ui` rather than a `Context`. A top-level panel gets its Ui
    // from a screen-sized background layer, per bevy_egui 0.41's side_panel
    // example. `default_size` is the OUTER width (frame margin included) where
    // `default_width` was inner — 320 is kept; the few pixels of drift are
    // invisible in a resizable panel.
    let mut viewport_ui = egui::Ui::new(
        ctx.clone(),
        "record_viewport".into(),
        egui::UiBuilder::new()
            .layer_id(egui::LayerId::background())
            .max_rect(ctx.viewport_rect()),
    );
    egui::Panel::left("record")
        .default_size(320.0)
        .show(&mut viewport_ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                let (built, noted) = identity(ui, &mut editor.record);
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
                rebuild |= body_axes(ui, &mut editor.record);
                rebuild |= skin_axes(ui, &mut editor.record);
                rebuild |= coat_axes(ui, &mut editor.record);
                rebuild |= fur_axes(ui, &mut editor.record);
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

/// A whole `i64` seed in a text field as wide as any `i64` (overlands
/// #1569). What is typed is written when the field lets go of the keyboard -
/// Enter, Tab or a click elsewhere - if it reads as an `i64` other than
/// `value`; a text that does not read is shown in the error colour while it
/// is typed and written nowhere. Whether it wrote `value`.
///
/// Not a drag number: egui's `DragValue` carries its value as an `f64`, so a
/// seed past 2^53 showed rounded, could not be dragged one step, and a click
/// in and out wrote it back rounded while reporting no change - so a host
/// marked nothing dirty and recorded no undo. A seed rolled from a hash spans
/// the whole range, so nearly every body met it. The text is the value's
/// whenever the field is not being typed into: a draft left unapplied is
/// dropped, not shown again later as if it were the value, and a value
/// changed underneath it (an undo, a re-roll) replaces it.
///
/// A draft is applied when focus leaves the field - Enter, Tab, a click
/// anywhere else - and dropped on Escape. A click on a widget drawn ABOVE
/// the field (the name box) moves focus before the field is drawn, so the
/// field cannot see itself lose it with the draft still in its text; it
/// remembers the pass it last held focus in and applies the draft then.
fn seed_field(ui: &mut egui::Ui, value: &mut i64) -> bool {
    /// The field's text while it is typed into, the value it was last read
    /// from, and the pass it last ended holding focus, if it did.
    #[derive(Clone)]
    struct SeedText {
        text: String,
        synced_to: i64,
        focused_in_pass: Option<u64>,
    }
    /// Write the draft, if it parses and differs; true when it did.
    fn apply(state: &mut SeedText, value: &mut i64) -> bool {
        match state.text.trim().parse::<i64>() {
            Ok(typed) if typed != *value => {
                *value = typed;
                state.synced_to = typed;
                true
            }
            _ => false,
        }
    }
    let id = ui.id().with("identity_seed");
    let edit_id = id.with("edit");
    let pass = ui.ctx().cumulative_pass_nr();
    let mut state = ui
        .data_mut(|d| d.get_temp::<SeedText>(id))
        .unwrap_or(SeedText {
            text: value.to_string(),
            synced_to: *value,
            focused_in_pass: None,
        });
    let focused = ui.memory(|m| m.has_focus(edit_id));
    // Focus left between the last pass and this one, before the field was
    // drawn - to a widget above it, or by Escape. Only the last pass counts:
    // a field hidden mid-draft and shown again later drops the draft.
    let left = !focused && state.focused_in_pass.is_some_and(|p| p + 1 == pass);
    let mut wrote =
        left && !ui.input(|i| i.key_pressed(egui::Key::Escape)) && apply(&mut state, value);
    if state.synced_to != *value || !focused {
        state.text = value.to_string();
        state.synced_to = *value;
    }
    let refused = state
        .text
        .trim()
        .parse::<i64>()
        .is_err()
        .then(|| ui.visuals().error_fg_color);
    let width = seed_field_width(ui);
    let response = ui
        .add(
            egui::TextEdit::singleline(&mut state.text)
                .id(edit_id)
                .desired_width(width)
                .text_color_opt(refused),
        )
        .on_hover_text("A whole number. Type one and press Enter to apply it.");
    if response.lost_focus() {
        wrote |= apply(&mut state, value);
    }
    state.focused_in_pass = response.has_focus().then_some(pass);
    ui.data_mut(|d| d.insert_temp(id, state));
    wrote
}

/// Wide enough for the longest `i64`, `i64::MIN`, in the field's own font:
/// its text, a `TextEdit`'s margin either side (its default,
/// `Margin::symmetric(4, 2)`), and room past the end for the cursor.
fn seed_field_width(ui: &egui::Ui) -> f32 {
    const MARGIN: f32 = 4.0;
    const CURSOR: f32 = 2.0;
    let font = egui::FontSelection::default().resolve(ui.style());
    let text = ui.fonts_mut(|fonts| {
        fonts
            .layout_no_wrap(i64::MIN.to_string(), font, egui::Color32::WHITE)
            .size()
            .x
    });
    text + 2.0 * MARGIN + CURSOR
}

/// Name, seed, archetype and the locks a re-roll honours.
///
/// Returns whether the body changed, and whether the record changed without
/// the body changing — two answers because they cost three orders of
/// magnitude apart, and a host that rebuilt for a keystroke in the name box
/// would pay a draft build per letter.
pub fn identity(ui: &mut egui::Ui, record: &mut AvatarRecord) -> (bool, bool) {
    let mut changed = false;
    let mut noted = false;
    ui.horizontal(|ui| {
        ui.label("name");
        noted |= ui
            .add(egui::TextEdit::singleline(&mut record.name).desired_width(200.0))
            .changed();
    });

    ui.horizontal(|ui| {
        ui.label("body");
        let humanoid = matches!(record.archetype, Archetype::Humanoid(_));
        if ui.selectable_label(humanoid, "humanoid").clicked() && !humanoid {
            record.archetype = Archetype::Humanoid(HumanoidParams::default());
            changed = true;
        }
        let quadruped = matches!(record.archetype, Archetype::Quadruped(_));
        if ui.selectable_label(quadruped, "quadruped").clicked() && !quadruped {
            record.archetype = Archetype::Quadruped(QuadrupedParams::default());
            changed = true;
        }
    });
    // Said where the body is chosen, because the sections it hides are
    // further down and their absence is otherwise a puzzle (#365) - naming
    // only the ones this body lacks.
    let anatomy = record.archetype.anatomy();
    let absent: Vec<&str> = [
        (!anatomy.human_head()).then_some("face"),
        (!anatomy.human_head()).then_some("hair"),
        (!anatomy.human_trunk()).then_some("outfit"),
    ]
    .into_iter()
    .flatten()
    .collect();
    if let Some((last, rest)) = absent.split_last() {
        let named = if rest.is_empty() {
            (*last).to_string()
        } else {
            format!("{} or {last}", rest.join(", "))
        };
        // A beast's fur is its own (symbios-avatar #369), and the hair lock
        // keeps it: said here, or the note and the lock row disagree.
        let fur = if matches!(record.archetype, Archetype::Quadruped(_)) {
            " Its fur has a section and a lock of its own."
        } else {
            ""
        };
        ui.small(format!(
            "This body has no {named} yet: those are a person's.{fur}"
        ));
    }

    ui.horizontal(|ui| {
        ui.label("seed");
        // Typing a seed records which draw a body came from; it does not
        // re-draw one. That is the button beside it.
        noted |= seed_field(ui, &mut record.seed);
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
            record.reroll(record.seed.wrapping_sub(1));
            changed = true;
        }
        if ui
            .button("▶")
            .on_hover_text("the seed after this")
            .clicked()
        {
            record.reroll(record.seed.wrapping_add(1));
            changed = true;
        }
    });

    // **What generation drew this body, and whether this build agrees**
    // (symbios-avatar #103, #169). A seed is stored so a look can be
    // reproduced, and that promise holds only against the generator that drew
    // it — the engine says so on `GENERATOR_VERSION` and then has no way to
    // tell anybody. This is the reader that should: a record rolled by an
    // older generation still LOADS, and every axis it stores is honoured
    // exactly, but pressing either seed arrow redraws it under today's rules.
    //
    // **"May", because a generation does not move every seed** (#48). Engine
    // generation 6 added a helmet coin that lands on one seed in ten and left
    // the other nine drawing what they drew under 5, so "will not reproduce"
    // was false nine times in ten - and this build cannot tell which seed is
    // which without re-rolling it.
    if record.generator != GENERATOR_VERSION {
        ui.horizontal_wrapped(|ui| {
            ui.small(format!(
                "⚠ rolled by generator {}, this build draws {} - the stored axes are \
                 exact, but re-rolling this seed may draw it differently",
                record.generator, GENERATOR_VERSION
            ));
        });
    }

    noted |= lock_row(ui, record);
    (changed, noted)
}

/// The lock toggles of [`identity`] and what the hunt is holding, returning
/// whether any lock was toggled. Its own function since the frame lock took a
/// name per plan (symbios-avatar #366), which took `identity` past its length.
fn lock_row(ui: &mut egui::Ui, record: &mut AvatarRecord) -> bool {
    let anatomy = record.archetype.anatomy();
    let frame = record.archetype.composite_priors().frame;
    // A beast rolls its fur under the hair lock (symbios-avatar #369).
    let furred = matches!(record.archetype, Archetype::Quadruped(_));
    let mut noted = false;
    ui.horizontal_wrapped(|ui| {
        ui.label("locked");
        for category in Category::ALL {
            // A lock on what this body never rolls holds nothing (#365).
            let rolled = match category {
                Category::Hair => anatomy.human_head() || furred,
                Category::Outfit => anatomy.human_trunk(),
                _ => true,
            };
            if !rolled {
                continue;
            }
            let mut locked = record.locks.is_locked(category);
            if ui
                .toggle_value(&mut locked, lock_name(category, frame, furred))
                .changed()
            {
                // A lock changes nothing about the body until the next
                // re-roll, so it is not a reason to rebuild one.
                record.locks.toggle(category);
                noted = true;
            }
        }
    });
    if record.locks.is_everything() {
        ui.small("every category locked: a re-roll would do nothing");
    } else {
        // What the hunt is actually searching, said plainly. Locking IS the
        // technique — keep what you have found, step the seed, judge only what
        // is still moving — and it stays invisible unless the panel says which
        // part of the body the next step is allowed to touch.
        let held = record.locks.locked();
        if !held.is_empty() {
            let names: Vec<&str> = held
                .into_iter()
                .map(|category| lock_name(category, frame, furred))
                .collect();
            ui.small(format!("hunting, holding {}", names.join(", ")));
        }
    }
    noted
}

/// The name to show a category by.
///
/// Eight of them, and head shape, complexion and hair are three rather than one
/// because they are what a creator most often wants to hold apart — a face kept
/// while its colouring is rolled.
#[must_use]
pub fn category_name(category: Category) -> &'static str {
    match category {
        Category::Stature => "stature",
        Category::Build => "build",
        Category::Frame => "frame",
        Category::Proportions => "proportions",
        Category::Head => "head",
        Category::Colouring => "colouring",
        Category::Hair => "hair",
        Category::Age => "age",
        Category::Outfit => "outfit",
    }
}

/// The name to show a lock by: its category's, except that on a body with no
/// frame axis the frame lock is "family", because what it keeps there is the
/// kind of animal the body is - its family, its cross, the blend - and its
/// body length (symbios-avatar #366); and on a furred body the hair lock is
/// "fur", which is what a re-roll draws under it there (symbios-avatar #369).
fn lock_name(category: Category, frame: bool, furred: bool) -> &'static str {
    match category {
        Category::Frame if !frame => "family",
        Category::Hair if furred => "fur",
        _ => category_name(category),
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
/// engine does not stretch them.
pub fn composite_axes(ui: &mut egui::Ui, record: &mut AvatarRecord) -> bool {
    use symbios_avatar::plan::{AGE_RANGE, BODY_FAT_RANGE};

    let mut changed = false;
    egui::CollapsingHeader::new("composites")
        .default_open(true)
        .show(ui, |ui| {
            // A person's axis (symbios-avatar #178): a body whose plan does not
            // read it is not offered it, and is told why where it would be.
            let frame = record.archetype.composite_priors().frame;
            let composites = &mut record.composites;
            if frame {
                changed |= explored(
                    ui,
                    "femininity",
                    &mut composites.femininity,
                    0.0,
                    (-1.0, 1.0),
                );
            } else {
                ui.small("This body has no frame axis: femininity is a person's.");
            }
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
///
/// Takes the whole record rather than the archetype (symbios-avatar #178): a
/// quadruped's family centres its composites as well as its own axes, so a
/// change of family carries both.
pub fn body_axes(ui: &mut egui::Ui, record: &mut AvatarRecord) -> bool {
    let AvatarRecord {
        archetype,
        composites,
        ..
    } = record;
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
                // The chest's five (symbios-avatar #273 and #289), and they are here
                // rather than with the composites for `head_breadth`'s reason:
                // this block is the per-region OFFSETS, and how much chest a
                // body has by default is `femininity`, `mass` and `bodyFat`'s
                // to say. **A chest is invisible on a dressed body** — the skin
                // under the clothes is not emitted — so a creator dragging
                // these with an outfit on sees nothing until the garment
                // catches up, which is worth knowing before it is reported as
                // a bug.
                changed |= explored(
                    ui,
                    "chest volume",
                    &mut params.chest_volume,
                    0.0,
                    (-1.0, 1.0),
                );
                changed |= explored(
                    ui,
                    "chest projection",
                    &mut params.chest_projection,
                    0.0,
                    (-1.0, 1.0),
                );
                changed |= explored(ui, "chest lift", &mut params.chest_lift, 0.0, (-1.0, 1.0));
                // The two milestone #9 added (symbios-avatar #289): where the
                // chest's volume sits rather than how much of it there is.
                // **Both are placements, so both are invisible on a flat
                // chest** — the engine scales the pair by the projection it
                // has, which is `femininity`, `mass` and `bodyFat`'s to say —
                // and `spacing` has a rail that moves with the body: a lobe may
                // come no closer to the sternum than 1.5 of its own spread, so
                // on a soft body the slider stops delivering before it stops
                // moving. That is the engine's `SPACING_FLOOR` and not a
                // clamp this panel can show.
                changed |= explored(
                    ui,
                    "chest spacing",
                    &mut params.chest_spacing,
                    0.0,
                    (-1.0, 1.0),
                );
                changed |= explored(
                    ui,
                    "chest fullness",
                    &mut params.chest_fullness,
                    0.0,
                    (-1.0, 1.0),
                );
            }
            Archetype::Quadruped(params) => changed |= quadruped_axes(ui, params, composites),
            // An archetype this build does not know about is kept verbatim
            // rather than edited into something else — a panel that offered
            // sliders for it would write a body over one it cannot read.
            Archetype::Unknown { type_name, .. } => {
                ui.small(format!("{type_name} is not an archetype this build knows"));
            }
        });
    changed
}

/// A quadruped's body: its family, and the axes rolled around it.
///
/// `build` and `muscle` used to sit here and are gone (symbios-avatar #178):
/// they retired into the `mass` and `bodyFat` composites, as the humanoid's
/// did, and the composites section above carries both.
fn quadruped_axes(
    ui: &mut egui::Ui,
    params: &mut QuadrupedParams,
    composites: &mut symbios_avatar::Composites,
) -> bool {
    let mut changed = family_picker(ui, params, composites);
    let stature = symbios_avatar::QuadrupedParams::height_envelope();
    changed |= axis(ui, "height", &mut params.height, stature.0..=stature.1);
    changed |= explored(ui, "body length", &mut params.body_length, 0.0, (-1.0, 1.0));
    changed |= explored(ui, "chest depth", &mut params.chest_depth, 0.0, (-1.0, 1.0));
    changed |= explored(ui, "leg length", &mut params.leg_length, 0.0, (-1.0, 1.0));
    changed |= explored(ui, "stance", &mut params.stance, 0.0, (-1.0, 1.0));
    changed |= chooser(ui, "feet", &mut params.feet, &Feet::ALL, Feet::name);
    changed |= explored(ui, "neck length", &mut params.neck_length, 0.0, (-1.0, 1.0));
    changed |= explored(
        ui,
        "neck carriage",
        &mut params.neck_carriage,
        0.0,
        (-1.0, 1.0),
    );
    changed |= explored(ui, "head size", &mut params.head_size, 0.0, (-1.0, 1.0));
    changed |= quadruped_head(ui, params);
    changed |= explored(ui, "tail length", &mut params.tail_length, 0.0, (-1.0, 1.0));
    changed |= explored(
        ui,
        "tail carriage",
        &mut params.tail_carriage,
        0.0,
        (-1.0, 1.0),
    );
    // Last, because it is drawn over everything above (symbios-avatar #366):
    // naturalistic to cute, the head, the face, the body and the legs at once.
    changed |= explored(ui, "appeal", &mut params.appeal, 0.0, (-1.0, 1.0));
    changed
}

/// A quadruped's face (symbios-avatar #367): the muzzle and how it is set,
/// the eyes, the ears, the nose, the whiskers and a horn.
///
/// The parts the engine stores rather than reads off an axis - the pupils,
/// the nose, a horn - are a row of choices each, where a creator overrides
/// what a family gave: a dog with a cat's eyes is a choice somebody made.
/// **Until the next family pick**, which hands the new family's own over
/// (`set_kind`) - only a part one of the body's families has survives one.
fn quadruped_head(ui: &mut egui::Ui, params: &mut QuadrupedParams) -> bool {
    let mut changed = false;
    for (name, value) in [
        ("muzzle length", &mut params.muzzle_length),
        ("muzzle breadth", &mut params.muzzle_breadth),
        ("stop", &mut params.stop),
        ("eye set", &mut params.eye_set),
    ] {
        changed |= explored(ui, name, value, 0.0, (-1.0, 1.0));
    }
    changed |= chooser(ui, "pupils", &mut params.pupil, &Pupil::ALL, Pupil::name);
    for (name, value) in [
        ("ear size", &mut params.ear_size),
        ("ear tips", &mut params.ear_point),
        ("ear carriage", &mut params.ear_carriage),
    ] {
        changed |= explored(ui, name, value, 0.0, (-1.0, 1.0));
    }
    changed |= chooser(ui, "nose", &mut params.nose, &Nose::ALL, Nose::name);
    changed |= explored(ui, "whiskers", &mut params.whiskers, 0.0, (-1.0, 1.0));
    changed |= chooser(ui, "horn", &mut params.horn, &Horn::ALL, Horn::name);
    changed
}

/// A stored part's choices as a row of labels, the current one selected
/// (symbios-avatar #367). Writes, and reports a change, only on a click that
/// picks something other than what is already there. The row wraps, since a
/// coat's pattern has eight (symbios-avatar #368).
fn chooser<T: Copy + PartialEq>(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut T,
    choices: &[T],
    name: fn(T) -> &'static str,
) -> bool {
    let mut changed = false;
    ui.horizontal_wrapped(|ui| {
        ui.label(label);
        for &choice in choices {
            if ui
                .selectable_label(*value == choice, name(choice))
                .clicked()
                && *value != choice
            {
                *value = choice;
                changed = true;
            }
        }
    });
    changed
}

/// Which kind of animal a quadruped is, and whether it is a cross
/// (symbios-avatar #366).
///
/// **A pick goes through `QuadrupedParams::set_kind`, never into the fields.**
/// The geometry reads only the axes, and a family is where a roll centres
/// them, so writing the field alone would change nothing on screen; `set_kind`
/// carries the body's character to the new centre instead - a long-legged dog
/// becomes a long-legged cat - and a cross's blend slider morphs the body
/// between its two families as it is dragged. Picking a cross starts it half
/// way, and the blend is only offered on a cross. The composites the family
/// centres are carried the same way (`CompositePriors::carry`).
fn family_picker(
    ui: &mut egui::Ui,
    params: &mut QuadrupedParams,
    composites: &mut symbios_avatar::Composites,
) -> bool {
    // **A drag re-kinds from where it began** (the P1 critic). Each frame of a
    // dragged blend used to re-centre from the last frame's body, and every
    // frame quantises, so the rounding summed over the drag: a dog crossed with
    // a horse dragged from a quarter to three quarters came out 6 cm taller
    // than one set there, and its body fat never moved at all. So the body as
    // it was when the gesture began is kept, and every frame of it re-kinds
    // from that: a drag lands where a jump would (#1599's lesson in the
    // overlands editor - remember what was asked, not what was last shown).
    let origin_id = ui.id().with("quadruped kind origin");
    let origin: Option<(QuadrupedParams, symbios_avatar::Composites)> =
        ui.data(|data| data.get_temp(origin_id));
    let (mut family, mut hybrid, mut blend) = (params.family, params.hybrid, params.blend);
    ui.horizontal(|ui| {
        ui.label("family");
        for kind in Family::ALL {
            if ui.selectable_label(family == kind, kind.name()).clicked() {
                family = kind;
            }
        }
    });
    ui.horizontal(|ui| {
        ui.label("crossed with");
        if ui.selectable_label(hybrid.is_none(), "none").clicked() {
            hybrid = None;
        }
        for kind in Family::ALL.into_iter().filter(|&kind| kind != family) {
            if ui
                .selectable_label(hybrid == Some(kind), kind.name())
                .clicked()
                && hybrid != Some(kind)
            {
                if hybrid.is_none() {
                    blend = 0.5;
                }
                hybrid = Some(kind);
            }
        }
    });
    let mut held = false;
    if hybrid.is_some() {
        // `axis`'s slider, kept here for the response a gesture is read from.
        let response = ui.add(
            egui::Slider::new(&mut blend, 0.0..=1.0)
                .text("blend")
                .step_by(0.001)
                .fixed_decimals(3),
        );
        held = response.dragged() || response.is_pointer_button_down_on();
    }
    let base = match origin {
        Some(origin) if held => origin,
        _ => (*params, *composites),
    };
    ui.data_mut(|data| {
        if held {
            data.insert_temp(origin_id, base);
        } else {
            data.remove::<(QuadrupedParams, symbios_avatar::Composites)>(origin_id);
        }
    });
    if (family, hybrid, blend) == (params.family, params.hybrid, params.blend) {
        return false;
    }
    // The family centres the composites too (symbios-avatar #178), so they are
    // carried with the plan's own axes: a slight dog becomes a slight horse.
    let (mut kind, mut carried) = base;
    let before = kind.composite_priors();
    kind.set_kind(family, hybrid, blend);
    before.carry(&kind.composite_priors(), &mut carried);
    *params = kind;
    *composites = carried;
    true
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
///
/// Drawn for every body painted from one, which a quadruped is not
/// (symbios-avatar #368): it wears a coat, [`coat_axes`], and its stored
/// complexion moves nothing on it.
pub fn skin_axes(ui: &mut egui::Ui, record: &mut AvatarRecord) -> bool {
    if matches!(record.archetype, Archetype::Quadruped(_)) {
        return false;
    }
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
        // **Stubble was the fifth slider here and is not a complexion axis any
        // more** (symbios-avatar #212). The painted hair layer replaced what it
        // drew, and it is asked for per region now — the `painted` slider and
        // its colour inside each of the five hair zones. The axis it moved had
        // been read by nothing since that landed, so this slider did nothing and
        // there was no way to tell from the panel that it would not.
    });
    changed
}

/// A quadruped's coat (symbios-avatar #368): its loci, how rich its red
/// runs, an unnatural palette, and the seed its marks are laid out from.
///
/// Drawn only for a quadruped, which wears a coat where a person wears a
/// complexion. The loci are a row of choices or a box each, as the stored
/// parts are, because the engine derives the colours from them: a liver dog
/// is the black dog's coat with one locus turned, and this is where it is
/// turned. The marks are a seed, thrown again rather than slid, because
/// neighbouring seeds lay out unrelated marks and a slider over them would
/// promise a nearness that is not there.
pub fn coat_axes(ui: &mut egui::Ui, record: &mut AvatarRecord) -> bool {
    let Archetype::Quadruped(params) = &mut record.archetype else {
        return false;
    };
    let coat = &mut params.coat;
    let mut changed = false;
    egui::CollapsingHeader::new("coat").show(ui, |ui| {
        changed |= chooser(
            ui,
            "pigment",
            &mut coat.pigment,
            &Pigment::ALL,
            Pigment::name,
        );
        changed |= ui.checkbox(&mut coat.dilute, "dilute").changed();
        changed |= chooser(ui, "agouti", &mut coat.agouti, &Agouti::ALL, Agouti::name);
        changed |= chooser(
            ui,
            "pattern",
            &mut coat.pattern,
            &Pattern::ALL,
            Pattern::name,
        );
        changed |= chooser(ui, "white", &mut coat.white, &White::ALL, White::name);
        ui.horizontal_wrapped(|ui| {
            for (name, locus) in [
                ("points", &mut coat.points),
                ("merle", &mut coat.merle),
                ("roan", &mut coat.roan),
                ("dun", &mut coat.dun),
                ("tortie", &mut coat.tortie),
            ] {
                changed |= ui.checkbox(locus, name).changed();
            }
        });
        changed |= explored(ui, "intensity", &mut coat.intensity, 0.0, (-1.0, 1.0));
        changed |= chooser(
            ui,
            "palette",
            &mut coat.fantasy,
            &Fantasy::ALL,
            Fantasy::name,
        );
        ui.horizontal(|ui| {
            ui.label(format!("marks {}", coat.marks));
            if ui.button("throw again").clicked() {
                coat.marks = next_marks(coat.marks);
                changed = true;
            }
        });
    });
    changed
}

/// A quadruped's fur (symbios-avatar #369): how long it grows in each zone
/// where a beast grows long hair, none at zero.
///
/// Drawn only for a quadruped. A slider each, over the engine's envelope for
/// a length: none to long, and past long into the exploration range, where
/// the engine's own ceiling thins the cards rather than letting the body
/// past its budget. And one that is not a length (symbios-avatar #389): how
/// stiff the mane is, flat at nothing and a short upright crest at the top,
/// over the engine's own stored range.
pub fn fur_axes(ui: &mut egui::Ui, record: &mut AvatarRecord) -> bool {
    let Archetype::Quadruped(params) = &mut record.archetype else {
        return false;
    };
    let fur = &mut params.fur;
    let mut changed = false;
    egui::CollapsingHeader::new("fur").show(ui, |ui| {
        for (name, length) in [
            ("mane", &mut fur.mane),
            ("ruff", &mut fur.ruff),
            ("tail", &mut fur.tail),
            ("feathering", &mut fur.feathering),
            ("ear tufts", &mut fur.tufts),
        ] {
            changed |= explored(ui, name, length, 0.0, (0.0, 1.0));
        }
        changed |= axis(ui, "mane stiffness", &mut fur.mane_stiffness, 0.0..=1.0);
    });
    changed
}

/// The marks seed thrown after `marks`: a step through all 65 536 that
/// visits every layout before it repeats one.
fn next_marks(marks: u16) -> u16 {
    // Full period modulo 2^16: the increment is odd and the multiplier one
    // more than a multiple of four.
    marks.wrapping_mul(0x9e35).wrapping_add(0x79b9)
}

/// How the eyes are shaped and set.
pub fn eye_axes(ui: &mut egui::Ui, record: &mut AvatarRecord) -> bool {
    let mut changed = false;
    egui::CollapsingHeader::new("eyes").show(ui, |ui| {
        changed |= explored(ui, "size", &mut record.eyes.size, 0.5, (0.0, 1.0));
        changed |= explored(ui, "spacing", &mut record.eyes.spacing, 0.0, (-1.0, 1.0));
        changed |= explored(ui, "depth", &mut record.eyes.depth, 0.0, (-1.0, 1.0));
        changed |= explored(ui, "aperture", &mut record.eyes.aperture, 0.8, (0.0, 1.0));
        // The iris fades inner to outer across the disc; the ring is the
        // circle around it (symbios-avatar #229).
        changed |= eye_colour(ui, "iris inner", &mut record.eyes.inner);
        changed |= eye_colour(ui, "iris outer", &mut record.eyes.outer);
        changed |= eye_colour(ui, "limbal ring", &mut record.eyes.ring);
    });
    changed
}

/// Nose, brow, mouth and ears.
///
/// Drawn only for a body whose head is a human skull (symbios-avatar #365):
/// on any other head nothing reads these axes, and a slider that moves
/// nothing is worse than none.
pub fn face_axes(ui: &mut egui::Ui, record: &mut AvatarRecord) -> bool {
    if !record.archetype.anatomy().human_head() {
        return false;
    }
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

/// Hair, which is five follicle regions rather than one head.
///
/// **One section per follicle region, each carrying both layers**, because that
/// is what the record now says: a region has a base style out of its own
/// catalogue, a cut, two colours to fade between, the hair painted into the skin
/// under it, and the shape parameters that say where on the head it grows at
/// all. The eight scalars this replaced described one sculpted shell and could
/// not say that a face has eyebrows.
///
/// The five are nested one level down rather than laid out flat: fifty-odd
/// controls in one open header is a wall, and a person editing a beard is not
/// editing a hairline. egui only runs the body of an OPEN header, so the four
/// nobody has opened cost nothing to draw.
///
/// Drawn only for a body whose head is a human skull, where every follicle
/// region is (symbios-avatar #365).
pub fn hair_axes(ui: &mut egui::Ui, record: &mut AvatarRecord) -> bool {
    if !record.archetype.anatomy().human_head() {
        return false;
    }
    let mut changed = false;
    egui::CollapsingHeader::new("hair").show(ui, |ui| {
        changed |= zone(ui, "scalp", |ui| {
            kept_unknown(
                ui,
                record.hair.scalp.style == symbios_avatar::ScalpStyle::None,
                record.hair.unrecognised.scalp.as_ref(),
            );
            let mut changed = scalp_style(ui, &mut record.hair.scalp.style);
            changed |= tress(ui, &mut record.hair.scalp);
            ui.separator();
            changed |= signed(ui, "hairline", &mut record.hair.regions.scalp.line);
            changed |= axis(
                ui,
                "temples",
                &mut record.hair.regions.scalp.temples,
                0.0..=1.0,
            );
            changed |= signed(ui, "nape", &mut record.hair.regions.scalp.nape);
            changed
        });
        changed |= zone(ui, "brows", |ui| {
            kept_unknown(
                ui,
                record.hair.brows.style == symbios_avatar::BrowStyle::None,
                record.hair.unrecognised.brows.as_ref(),
            );
            let mut changed = brow_style(ui, &mut record.hair.brows.style);
            changed |= tress(ui, &mut record.hair.brows);
            ui.separator();
            changed |= signed(ui, "rise", &mut record.hair.regions.brows.rise);
            changed |= signed(ui, "apart", &mut record.hair.regions.brows.apart);
            changed |= signed(ui, "reach", &mut record.hair.regions.brows.reach);
            changed |= signed(ui, "arch", &mut record.hair.regions.brows.arch);
            changed
        });
        changed |= zone(ui, "moustache", |ui| {
            kept_unknown(
                ui,
                record.hair.moustache.style == symbios_avatar::MoustacheStyle::None,
                record.hair.unrecognised.moustache.as_ref(),
            );
            let mut changed = moustache_style(ui, &mut record.hair.moustache.style);
            changed |= tress(ui, &mut record.hair.moustache);
            ui.separator();
            changed |= signed(ui, "width", &mut record.hair.regions.moustache.width);
            changed |= signed(ui, "drop", &mut record.hair.regions.moustache.drop);
            changed
        });
        changed |= zone(ui, "chin", |ui| {
            kept_unknown(
                ui,
                record.hair.chin.style == symbios_avatar::ChinStyle::None,
                record.hair.unrecognised.chin.as_ref(),
            );
            let mut changed = chin_style(ui, &mut record.hair.chin.style);
            changed |= tress(ui, &mut record.hair.chin);
            ui.separator();
            changed |= signed(ui, "width", &mut record.hair.regions.chin.width);
            changed |= signed(ui, "under", &mut record.hair.regions.chin.under);
            changed |= signed(ui, "rise", &mut record.hair.regions.chin.rise);
            changed
        });
        changed |= zone(ui, "flanks", |ui| {
            kept_unknown(
                ui,
                record.hair.flanks.style == symbios_avatar::FlankStyle::None,
                record.hair.unrecognised.flanks.as_ref(),
            );
            let mut changed = flank_style(ui, &mut record.hair.flanks.style);
            changed |= tress(ui, &mut record.hair.flanks);
            ui.separator();
            changed |= signed(ui, "cheek", &mut record.hair.regions.flanks.cheek);
            changed |= signed(ui, "under", &mut record.hair.regions.flanks.under);
            changed |= signed(ui, "sideburn", &mut record.hair.regions.flanks.sideburn);
            changed
        });
    });
    changed
}

/// One region's header, whose body only runs when it is open.
fn zone(ui: &mut egui::Ui, name: &str, body: impl FnOnce(&mut egui::Ui) -> bool) -> bool {
    egui::CollapsingHeader::new(name)
        .show(ui, body)
        .body_returned
        .unwrap_or(false)
}

/// The half of a region that is the same whatever grows there: how it is cut,
/// the two colours it fades between, and the hair painted into the skin.
///
/// Generic over the region's style for the same reason `Tress` is: a bob is not
/// a thing a chin can have, so the styles are five types — but everything else
/// about a region is one type with five parameters, and writing this out five
/// times is five chances for one of them to drift.
fn tress<S: symbios_avatar::hair::Style>(
    ui: &mut egui::Ui,
    tress: &mut symbios_avatar::hair::Tress<S>,
) -> bool {
    let mut changed = axis(ui, "length", &mut tress.cut.length, 0.0..=1.0);
    changed |= axis(ui, "thickness", &mut tress.cut.thickness, 0.0..=1.0);
    changed |= axis(ui, "density", &mut tress.cut.density, 0.0..=1.0);
    changed |= axis(ui, "droop", &mut tress.cut.droop, 0.0..=1.0);
    changed |= hair_colour(ui, "roots", &mut tress.roots);
    changed |= hair_colour(ui, "tips", &mut tress.tips);
    changed |= axis(ui, "painted", &mut tress.skin.density, 0.0..=1.0);
    changed |= hair_colour(ui, "paint", &mut tress.skin.colour);
    changed
}

/// One hair colour: the engine's own natural ramp, and a free picker beside it.
///
/// **Both, and the pair is the point.** The melanin ramp
/// is what a natural head of hair sits on and it is drawn here from the engine's
/// own function rather than from a palette invented in this crate, so a swatch
/// cannot drift from the hair it paints — the same argument the complexion row
/// makes. But the ramp's light end is a warm blonde, so it cannot say grey, and
/// it cannot say green either; the record stores free sRGB precisely so that it
/// can, and a panel offering only the ramp would put half the record out of
/// reach the way a `0..=1` slider puts an axis with a wider range out of reach.
///
/// **The picker is sRGB on both sides.** `Color32` is sRGB bytes and the record
/// is sRGB thousandths, so nothing here converts — which is the whole trap this
/// crate has already paid for once in the other direction, where copying the
/// engine's sRGB vertex colours into a linear channel drew dark hair as milk
/// chocolate. The one loss is precision: a byte is coarser than a thousandth,
/// so a colour that has been through the picker lands on a 1/255 step. It is
/// only written back when the picker actually changed, so a colour nobody
/// touches keeps the value the record was loaded with.
fn hair_colour(ui: &mut egui::Ui, name: &str, colour: &mut [f32; 3]) -> bool {
    ramped_colour(
        ui,
        name,
        colour,
        symbios_avatar::hair::style::melanin,
        "melanin",
    )
}

/// One iris colour: the engine's pigment ramp, and a free picker beside it.
///
/// The same pair [`hair_colour`] offers, for the same two reasons: the ramp is
/// drawn from the engine's own
/// `iris_pigment` so a swatch cannot drift from the iris a roll paints, and the
/// free picker reaches the fantasy colours the record's channels deliberately
/// leave open.
fn eye_colour(ui: &mut egui::Ui, name: &str, colour: &mut [f32; 3]) -> bool {
    ramped_colour(
        ui,
        name,
        colour,
        symbios_avatar::face::eye::iris_pigment,
        "pigment",
    )
}

/// The widget both of the above are: engine ramp swatches, then a free picker.
fn ramped_colour(
    ui: &mut egui::Ui,
    name: &str,
    colour: &mut [f32; 3],
    ramp: impl Fn(f32) -> [f32; 3],
    hover: &str,
) -> bool {
    let mut changed = false;
    ui.horizontal_wrapped(|ui| {
        ui.label(name);
        changed |= swatch_row(
            ui,
            colour,
            (0..=8u16).map(|stop| f32::from(stop) / 8.0),
            ramp,
            hover,
        );
        changed |= free_colour(ui, colour);
    });
    changed
}

/// A row of swatches sampled from `ramp` at `stops`, one click each to wear.
///
/// The swatch a colour sits on is marked, within a quarter of a byte, which is
/// the picker's own precision.
fn swatch_row(
    ui: &mut egui::Ui,
    colour: &mut [f32; 3],
    stops: impl IntoIterator<Item = f32>,
    ramp: impl Fn(f32) -> [f32; 3],
    hover: &str,
) -> bool {
    let mut changed = false;
    for at in stops {
        let tone = ramp(at);
        let picked = tone
            .iter()
            .zip(colour.iter())
            .all(|(a, b)| (a - b).abs() < 0.004);
        let button = egui::Button::new(if picked { "•" } else { " " })
            .fill(swatch(symbios_avatar::Vec3::from_array(tone)))
            .min_size(egui::vec2(18.0, 18.0));
        if ui
            .add(button)
            .on_hover_text(format!("{hover} {at:.2}"))
            .clicked()
        {
            *colour = tone;
            changed = true;
        }
    }
    changed
}

/// The free sRGB picker beside a row of swatches.
///
/// Written back only when the picker changed, so a colour nobody touches keeps
/// the value the record was loaded with rather than the byte the picker holds.
fn free_colour(ui: &mut egui::Ui, colour: &mut [f32; 3]) -> bool {
    let mut free = swatch(symbios_avatar::Vec3::from_array(*colour));
    let changed = ui
        .color_edit_button_srgba(&mut free)
        .on_hover_text("any colour, sRGB")
        .changed();
    if changed {
        *colour = [
            f32::from(free.r()) / 255.0,
            f32::from(free.g()) / 255.0,
            f32::from(free.b()) / 255.0,
        ];
    }
    changed
}

/// A region's styles in two families, each row behind a small label of its
/// own: the card styles, and the helmet family (#48).
///
/// Picking a style hands back a fresh instance of it rather than trying to carry
/// an axis across: a bob's fringe and a tail's height are different quantities
/// that happen to be spelled the same way, and carrying one into the other is
/// how a panel writes a haircut nobody asked for.
///
/// Two rows rather than one long one, because the families draw differently
/// enough that a person picking one wants to know which they are in: a card
/// style is locks with a fringe of their own, a helmet is one closed solid.
fn families<S: Copy>(
    ui: &mut egui::Ui,
    current: &mut S,
    cards: &[(&str, S)],
    helmets: &[(&str, S)],
) -> bool {
    let mut changed = false;
    for (family, choices) in [("cards", cards), ("helmet", helmets)] {
        ui.horizontal_wrapped(|ui| {
            ui.small(family);
            for (name, style) in choices {
                // By variant, not by value: a bob at fringe 0.8 is still the bob,
                // and comparing whole values left it unmarked and let a click on
                // it reset the fringe to the middle.
                let picked = std::mem::discriminant(current) == std::mem::discriminant(style);
                if ui.selectable_label(picked, *name).clicked() && !picked {
                    *current = *style;
                    changed = true;
                }
            }
        });
    }
    changed
}

/// A line saying a region's record names a style this build cannot draw.
///
/// **Painted, not hovered** (#48). Since engine 0.9 a record from a newer build
/// that names a style this one does not know still loads: the region reads as
/// `none` - which is what the row below shows as picked - and the style object
/// is kept and written back unchanged for as long as the region stays `none`,
/// so saving the record does not delete somebody's haircut. Without this line
/// the panel would show a bald region and say nothing, and picking any style
/// replaces what was kept, so the person about to do that is the one who needs
/// telling. Shown only while the region is `none`, because that is the only
/// time the kept style is written.
fn kept_unknown(
    ui: &mut egui::Ui,
    none: bool,
    kept: Option<&serde_json::Map<String, serde_json::Value>>,
) {
    if let Some(kept) = kept.filter(|_| none) {
        let name = kept
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unnamed");
        ui.small(format!(
            "this record wears a style this build cannot draw ({name}); it is kept as it is \
             unless you pick another here"
        ));
    }
}

/// The scalp's card styles, each at the middle of its own axis, in the engine's
/// declaration order.
const SCALP_CARDS: [(&str, symbios_avatar::ScalpStyle); 6] = {
    use symbios_avatar::ScalpStyle as S;
    [
        ("none", S::None),
        ("crop", S::Crop),
        ("bob", S::Bob { fringe: 0.5 }),
        ("long", S::Long { weight: 0.5 }),
        ("tied", S::TiedBack { tail: 0.5 }),
        ("curly", S::Curly { curl: 0.5 }),
    ]
};

/// The scalp's helmet family (engine 0.9, #48): closed solids rather than cards.
const SCALP_HELMETS: [(&str, symbios_avatar::ScalpStyle); 7] = {
    use symbios_avatar::ScalpStyle as S;
    [
        ("cap", S::Cap { fringe: 0.5 }),
        ("slick back", S::SlickBack { volume: 0.5 }),
        ("bell", S::Bell { length: 0.5 }),
        ("bun", S::Bun { height: 0.5 }),
        ("crest", S::Crest { height: 0.5 }),
        ("afro", S::Afro { size: 0.5 }),
        ("braids", S::Braids { rows: 0.5 }),
    ]
};

/// The brows' card styles.
const BROW_CARDS: [(&str, symbios_avatar::BrowStyle); 3] = {
    use symbios_avatar::BrowStyle as S;
    [
        ("none", S::None),
        ("natural", S::Natural),
        ("thick", S::Thick),
    ]
};

/// The brows' sculpted style.
const BROW_HELMETS: [(&str, symbios_avatar::BrowStyle); 1] =
    [("sculpted", symbios_avatar::BrowStyle::Sculpted)];

/// The upper lip's card styles.
const MOUSTACHE_CARDS: [(&str, symbios_avatar::MoustacheStyle); 4] = {
    use symbios_avatar::MoustacheStyle as S;
    [
        ("none", S::None),
        ("chevron", S::Chevron),
        ("handlebar", S::Handlebar { sweep: 0.5 }),
        ("pencil", S::Pencil { ride: 0.5 }),
    ]
};

/// The upper lip's sculpted style.
const MOUSTACHE_HELMETS: [(&str, symbios_avatar::MoustacheStyle); 1] = [(
    "sculpted",
    symbios_avatar::MoustacheStyle::Sculpted { flare: 0.5 },
)];

/// The chin's card styles.
const CHIN_CARDS: [(&str, symbios_avatar::ChinStyle); 4] = {
    use symbios_avatar::ChinStyle as S;
    [
        ("none", S::None),
        ("goatee", S::Goatee { point: 0.5 }),
        ("full", S::Full),
        ("braided", S::Braided { twist: 0.5 }),
    ]
};

/// The chin's sculpted style.
const CHIN_HELMETS: [(&str, symbios_avatar::ChinStyle); 1] = [(
    "sculpted",
    symbios_avatar::ChinStyle::Sculpted { length: 0.5 },
)];

/// The jaw flanks' card styles.
const FLANK_CARDS: [(&str, symbios_avatar::FlankStyle); 3] = {
    use symbios_avatar::FlankStyle as S;
    [
        ("none", S::None),
        ("sideburns", S::Sideburns { drop: 0.5 }),
        ("full connect", S::FullConnect { reach: 0.5 }),
    ]
};

/// The jaw flanks' sculpted style.
const FLANK_HELMETS: [(&str, symbios_avatar::FlankStyle); 1] =
    [("sculpted", symbios_avatar::FlankStyle::Sculpted)];

/// The scalp's catalogue, and whichever axis the picked style carries.
fn scalp_style(ui: &mut egui::Ui, style: &mut symbios_avatar::ScalpStyle) -> bool {
    use symbios_avatar::ScalpStyle as S;
    let mut changed = families(ui, style, &SCALP_CARDS, &SCALP_HELMETS);
    // Matched rather than looked up, because the axis's NAME is part of the
    // style: "fringe" and "tail" mean different things and a panel that called
    // both of them "axis" would be a panel nobody can use.
    changed |= match style {
        S::None | S::Crop => false,
        S::Bob { fringe } => axis(ui, "fringe", fringe, 0.0..=1.0),
        S::Long { weight } => axis(ui, "back weight", weight, 0.0..=1.0),
        S::TiedBack { tail } => axis(ui, "tail height", tail, 0.0..=1.0),
        S::Curly { curl } => axis(ui, "curl", curl, 0.0..=1.0),
        // The helmet family's axes, each named for what it moves rather than
        // for its field: "length" is already the cut's slider just below, and a
        // bell's is where the hem falls.
        S::Cap { fringe } => axis(ui, "fringe notch", fringe, 0.0..=1.0),
        S::SlickBack { volume } => axis(ui, "volume", volume, 0.0..=1.0),
        S::Bell { length } => axis(ui, "hem length", length, 0.0..=1.0),
        S::Bun { height } => axis(ui, "bun height", height, 0.0..=1.0),
        S::Crest { height } => axis(ui, "crest height", height, 0.0..=1.0),
        S::Afro { size } => axis(ui, "size", size, 0.0..=1.0),
        S::Braids { rows } => axis(ui, "rows", rows, 0.0..=1.0),
    };
    changed
}

/// The brows' catalogue, which carries no axis of its own.
fn brow_style(ui: &mut egui::Ui, style: &mut symbios_avatar::BrowStyle) -> bool {
    families(ui, style, &BROW_CARDS, &BROW_HELMETS)
}

/// The upper lip's catalogue.
fn moustache_style(ui: &mut egui::Ui, style: &mut symbios_avatar::MoustacheStyle) -> bool {
    use symbios_avatar::MoustacheStyle as S;
    let mut changed = families(ui, style, &MOUSTACHE_CARDS, &MOUSTACHE_HELMETS);
    changed |= match style {
        S::None | S::Chevron => false,
        S::Handlebar { sweep } => axis(ui, "sweep", sweep, 0.0..=1.0),
        S::Pencil { ride } => axis(ui, "ride", ride, 0.0..=1.0),
        S::Sculpted { flare } => axis(ui, "flare", flare, 0.0..=1.0),
    };
    changed
}

/// The chin's catalogue.
fn chin_style(ui: &mut egui::Ui, style: &mut symbios_avatar::ChinStyle) -> bool {
    use symbios_avatar::ChinStyle as S;
    let mut changed = families(ui, style, &CHIN_CARDS, &CHIN_HELMETS);
    changed |= match style {
        S::None | S::Full => false,
        S::Goatee { point } => axis(ui, "point", point, 0.0..=1.0),
        S::Braided { twist } => axis(ui, "twist", twist, 0.0..=1.0),
        // Named for what it moves: "length" is the cut's slider just below.
        S::Sculpted { length } => axis(ui, "hang", length, 0.0..=1.0),
    };
    changed
}

/// The jaw flanks' catalogue.
fn flank_style(ui: &mut egui::Ui, style: &mut symbios_avatar::FlankStyle) -> bool {
    use symbios_avatar::FlankStyle as S;
    let mut changed = families(ui, style, &FLANK_CARDS, &FLANK_HELMETS);
    changed |= match style {
        S::None | S::Sculpted => false,
        S::Sideburns { drop } => axis(ui, "drop", drop, 0.0..=1.0),
        S::FullConnect { reach } => axis(ui, "reach", reach, 0.0..=1.0),
    };
    changed
}

/// What the body is wearing: a top and a pair of trousers, each a colour, a
/// length and optionally a texture (symbios-avatar #354).
///
/// One nested header per garment, the hair section's shape and for its
/// reason: a texture's own panel is a dozen controls, and two of them open at
/// once is a wall.
///
/// Drawn only for a body with a human trunk, which the garments are cut for
/// (symbios-avatar #365).
pub fn outfit_axes(ui: &mut egui::Ui, record: &mut AvatarRecord) -> bool {
    if !record.archetype.anatomy().human_trunk() {
        return false;
    }
    let mut changed = false;
    egui::CollapsingHeader::new("outfit").show(ui, |ui| {
        changed |= zone(ui, "top", |ui| {
            garment(
                ui,
                "top",
                &mut record.outfit.top,
                dress::SLEEVE_RANGE,
                &SLEEVE_PRESETS,
            )
        });
        changed |= zone(ui, "trousers", |ui| {
            garment(
                ui,
                "trousers",
                &mut record.outfit.trousers,
                dress::TROUSER_RANGE,
                &LEG_PRESETS,
            )
        });
    });
    changed
}

/// A top's lengths with names of their own, offered as buttons beside the
/// slider: the old cuts (symbios-avatar #356) and the two between them a
/// person names when they talk about a shirt.
const SLEEVE_PRESETS: [(&str, f32); 5] = [
    ("sleeveless", 0.0),
    ("short", 0.25),
    ("elbow", 0.5),
    ("forearm", dress::FOREARM),
    ("wrist", 1.0),
];

/// Trousers' lengths with names of their own, likewise.
const LEG_PRESETS: [(&str, f32); 4] = [
    ("shorts", dress::SHORTS),
    ("knee", 0.5),
    ("calf", dress::CALF),
    ("ankle", 1.0),
];

/// One garment: its colour, its length, and what it is made of.
///
/// **The length is a share of the limb, and the slider says so**: `0.5` is the
/// elbow or the knee on every body, whatever its proportions, and the hem lands
/// where the value says rather than on the nearest ring of faces (#356). The
/// named lengths beside it are the ones a person asks for by name.
fn garment(
    ui: &mut egui::Ui,
    id: &str,
    worn: &mut GarmentParams,
    range: (f32, f32),
    presets: &[(&str, f32)],
) -> bool {
    let mut changed = garment_colour(ui, "colour", &mut worn.colour);
    ui.horizontal_wrapped(|ui| {
        ui.small("length");
        for &(name, length) in presets {
            // Against a thousandth, which is the grid the record holds a length
            // on: a preset the slider has landed on exactly is that preset.
            let picked = (worn.length - length).abs() < 5e-4;
            if ui.selectable_label(picked, name).clicked() && !picked {
                worn.length = length;
                changed = true;
            }
        }
    });
    changed |= axis(ui, "length", &mut worn.length, range.0..=range.1);
    changed |= garment_texture(ui, id, &mut worn.texture);
    changed
}

/// A garment's colour: quick picks from the engine's own dye wheel and a grey
/// ramp, and a free picker beside them.
///
/// **Free first, the swatches a convenience.** The record stores any sRGB
/// colour (#355) — the hue-and-shade it replaced could not say black, white or
/// anything saturated — so the picker is the control and the rows are a start:
/// the wheel is `dress::dye` at a middle shade, the dyed-cloth colours the old
/// axes reached, and the ramp is the neutrals a wheel has no room for.
fn garment_colour(ui: &mut egui::Ui, name: &str, colour: &mut [f32; 3]) -> bool {
    let mut changed = false;
    ui.horizontal_wrapped(|ui| {
        ui.label(name);
        // Eight hues round the wheel — its ninth stop would be its first —
        // and nine greys from black to white.
        changed |= swatch_row(
            ui,
            colour,
            (0..8u16).map(|stop| f32::from(stop) / 8.0),
            |hue| dress::dye(hue, 0.45),
            "dye",
        );
        changed |= swatch_row(
            ui,
            colour,
            (0..=8u16).map(|stop| f32::from(stop) / 8.0),
            |level| [level; 3],
            "grey",
        );
        changed |= free_colour(ui, colour);
    });
    changed
}

/// What a garment is made of: plain cloth, or one of the tileable surfaces
/// `symbios-texture` generates, at a tile size and a turn, with the
/// generator's own panel under it.
///
/// **The generator's panel is that crate's, not a copy** (#50): it is
/// generated from the same per-field registry as the config, so every range it
/// offers is the envelope the record's `sanitize` clamps to and a slider never
/// shows a value the record cannot hold.
fn garment_texture(ui: &mut egui::Ui, id: &str, texture: &mut Option<GarmentTexture>) -> bool {
    let mut changed = false;
    let current = texture.as_ref().map_or_else(
        || String::from("plain"),
        |worn| worn.surface.name().to_owned(),
    );
    egui::ComboBox::from_id_salt(("garment texture", id))
        .selected_text(format!("made of: {current}"))
        .show_ui(ui, |ui| {
            if ui.selectable_label(texture.is_none(), "plain").clicked() && texture.is_some() {
                *texture = None;
                changed = true;
            }
            for &name in SurfaceConfig::NAMES {
                let picked = current == name;
                if ui.selectable_label(picked, name).clicked()
                    && !picked
                    && let Some(surface) = SurfaceConfig::named(name)
                {
                    // The tile size and the turn carry across a change of
                    // surface: they are the garment's, not the surface's.
                    *texture = Some(match texture.take() {
                        Some(old) => GarmentTexture { surface, ..old },
                        None => GarmentTexture::new(surface),
                    });
                    changed = true;
                }
            }
        });
    let Some(worn) = texture else {
        return changed;
    };
    if !worn.surface.is_known() {
        // Painted, not hovered, for the reason `kept_unknown` gives.
        ui.small(format!(
            "this garment is made of a surface this build cannot draw ({current}); it is kept as \
             it is unless you pick another here, and drawn in its plain colour until then"
        ));
        return changed;
    }
    // Logarithmic, and without a step: a range from a two-centimetre tile to a
    // four-metre one is three decades, and a stepped slider would rewrite a
    // record's value on sight. The host's `sanitize` puts it on the grid.
    changed |= ui
        .add(
            egui::Slider::new(
                &mut worn.scale,
                dress::surface::SCALE_RANGE.0..=dress::surface::SCALE_RANGE.1,
            )
            .logarithmic(true)
            .text("tiles per metre"),
        )
        .changed();
    changed |= axis(
        ui,
        "turn",
        &mut worn.rotation,
        dress::surface::ROTATION_RANGE.0..=dress::surface::ROTATION_RANGE.1,
    );
    if let Some(mut config) = texture_config(&worn.surface) {
        let (writeback, _) = bevy_symbios_texture::ui::texture_config_editor(
            ui,
            &mut config,
            egui::Id::new(("garment surface", id)),
        );
        if writeback && let Some(surface) = surface_config(&config) {
            worn.surface = surface;
            changed = true;
        }
    }
    changed
}

/// A garment's surface as the texture crate's own tagged config, for its
/// panel. Both are the registry's configs under the same `$type` names, so the
/// native JSON of one is the other.
fn texture_config(surface: &SurfaceConfig) -> Option<bevy_symbios_texture::TextureConfig> {
    let mut native = surface.to_native()?;
    native
        .as_object_mut()?
        .insert("$type".into(), serde_json::Value::from(surface.name()));
    serde_json::from_value(native).ok()
}

/// And back.
fn surface_config(config: &bevy_symbios_texture::TextureConfig) -> Option<SurfaceConfig> {
    let mut native = serde_json::to_value(config).ok()?;
    let name = native
        .as_object_mut()?
        .remove("$type")?
        .as_str()?
        .to_owned();
    SurfaceConfig::from_native(&name, native)
}

/// A `0..=1`-ish axis on a slider that steps in thousandths.
///
/// A thousandth is exactly what the wire format carries, so the slider cannot
/// land between two values a record can hold. What it shows is what would be
/// written.
pub fn axis(
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
pub fn signed(ui: &mut egui::Ui, name: &str, value: &mut f32) -> bool {
    axis(ui, name, value, -1.0..=1.0)
}

/// A shape axis over its exploration envelope.
///
/// The range comes from the engine's own [`explore_range`] over the axis's
/// default and conservative span, so the sliders and `sanitize` cannot
/// disagree about where an axis ends. Style axes — complexion, hair, outfit —
/// deliberately keep their classic sliders; the envelope is a shape idea.
///
/// [`explore_range`]: symbios_avatar::plan::explore_range
pub fn explored(
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
/// measured body is the error that once cost a session, where a shoulder was
/// pushed out to clear a ribcage half again wider than the visible one. And the
/// fractions are of NOMINAL stature, not of the built body's height, because
/// a fraction of rendered height silently changes whenever anything moves the
/// head — which is how a band figure goes stale without a coefficient being
/// touched.
///
/// Costs nothing while the header is shut: egui only runs the body of an open
/// one, and the skeleton is rebuilt inside it rather than cached, so what is
/// shown cannot lag what is set.
pub fn derived(ui: &mut egui::Ui, record: &AvatarRecord) {
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

        // **Nominal against built, because age put a wedge between them**
        // (symbios-avatar #167, #103). The settle takes its length out of the
        // trunk, so an old body stands shorter than the stature axis says
        // while every other readout here stays a fraction of the nominal
        // figure. One line said "nominal" and left the difference for somebody
        // to discover; now the panel shows both whenever they disagree.
        let built = skeleton
            .nodes
            .iter()
            .map(|node| node.position.y + node.radius)
            .fold(f32::MIN, f32::max);
        ui.small(format!("stature      {stature:.3} m nominal"));
        if (built - stature).abs() > 0.002 {
            ui.small(format!(
                "             {built:.3} m to the crown of the cage, {:+.1} cm",
                (built - stature) * 100.0
            ));
        }
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
    use symbios_avatar::{Coat, Fur, LockSet};

    /// A quadruped's axes, for [`axes`]: its own list, since a beast's
    /// body, face, coat and fur put [`axes`] past a hundred lines.
    fn beast_axes(p: &QuadrupedParams) -> Vec<(&'static str, f32)> {
        vec![
            ("height", p.height),
            ("body_length", p.body_length),
            ("leg_length", p.leg_length),
            ("neck_length", p.neck_length),
            ("head_size", p.head_size),
            ("tail_length", p.tail_length),
            // symbios-avatar #366: the cross's blend and the six axes
            // the families brought.
            ("blend", p.blend),
            ("muzzle_length", p.muzzle_length),
            ("stance", p.stance),
            ("neck_carriage", p.neck_carriage),
            ("tail_carriage", p.tail_carriage),
            ("chest_depth", p.chest_depth),
            ("appeal", p.appeal),
            // symbios-avatar #367: the face's.
            ("muzzle_breadth", p.muzzle_breadth),
            ("stop", p.stop),
            ("eye_set", p.eye_set),
            ("ear_size", p.ear_size),
            ("ear_point", p.ear_point),
            ("ear_carriage", p.ear_carriage),
            ("whiskers", p.whiskers),
            // symbios-avatar #368: the coat's one axis.
            ("coat intensity", p.coat.intensity),
            // symbios-avatar #369: the fur's five, and #389's stiffness.
            ("fur mane", p.fur.mane),
            ("fur ruff", p.fur.ruff),
            ("fur tail", p.fur.tail),
            ("fur feathering", p.fur.feathering),
            ("fur tufts", p.fur.tufts),
            ("fur mane stiffness", p.fur.mane_stiffness),
        ]
    }

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
                ("chest_volume", p.chest_volume),
                ("chest_projection", p.chest_projection),
                ("chest_lift", p.chest_lift),
                ("chest_spacing", p.chest_spacing),
                ("chest_fullness", p.chest_fullness),
            ],
            Archetype::Quadruped(p) => beast_axes(p),
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
            // The outfit (symbios-avatar #354): two garments of a colour and a
            // length, where there were four hue and shade axes.
            ("top red", record.outfit.top.colour[0]),
            ("top green", record.outfit.top.colour[1]),
            ("top blue", record.outfit.top.colour[2]),
            ("top length", record.outfit.top.length),
            ("trousers red", record.outfit.trousers.colour[0]),
            ("trousers green", record.outfit.trousers.colour[1]),
            ("trousers blue", record.outfit.trousers.colour[2]),
            ("trousers length", record.outfit.trousers.length),
        ]);
        // A texture's own two axes, where a garment wears one. Its surface's
        // config rides the wire in ten-thousandths rather than thousandths,
        // so it is not an axis here; the whole-record equality in the round
        // trip below is what covers it.
        for (name, garment) in [
            ("top", &record.outfit.top),
            ("trousers", &record.outfit.trousers),
        ] {
            if let Some(texture) = &garment.texture {
                out.push((
                    if name == "top" {
                        "top tiles"
                    } else {
                        "trousers tiles"
                    },
                    texture.scale,
                ));
                out.push((
                    if name == "top" {
                        "top turn"
                    } else {
                        "trousers turn"
                    },
                    texture.rotation,
                ));
            }
        }
        // **Five regions of the same shape, pushed rather than listed**
        // (symbios-avatar #202/#209). Hair used to be eight scalars on one
        // struct and they were written out one per line here; it is now five
        // tresses of four cut axes, three colours of three channels and a paint
        // density, plus the region's own shape axes — a hundred and ten numbers,
        // and a hand-written list of a hundred and ten is a list with something
        // missing from it. The list this replaced was already one short: the
        // panel wrote `hair.locks` through `counts` and every colour the record
        // carried was invisible to this test.
        for (name, tress) in hair_axes_of(record) {
            out.push((name, tress));
        }
        out
    }

    /// Every hair number the panel writes, region by region.
    ///
    /// The names are leaked deliberately: this is a test helper building a list
    /// of `&'static str` for a suite that runs once, and threading a lifetime
    /// through it to save a few dozen bytes would be the tail wagging the dog.
    fn hair_axes_of(record: &AvatarRecord) -> Vec<(&'static str, f32)> {
        let mut out = Vec::new();
        let hair = &record.hair;
        {
            let mut region = |name: &str,
                              cut: symbios_avatar::Cut,
                              roots: [f32; 3],
                              tips: [f32; 3],
                              paint: symbios_avatar::Paint| {
                let at = |what: &str| -> &'static str {
                    Box::leak(format!("hair {name} {what}").into_boxed_str())
                };
                out.push((at("length"), cut.length));
                out.push((at("thickness"), cut.thickness));
                out.push((at("density"), cut.density));
                out.push((at("droop"), cut.droop));
                for (channel, at) in ["r", "g", "b"].into_iter().enumerate() {
                    out.push((
                        Box::leak(format!("hair {name} roots {at}").into_boxed_str()),
                        roots[channel],
                    ));
                    out.push((
                        Box::leak(format!("hair {name} tips {at}").into_boxed_str()),
                        tips[channel],
                    ));
                    out.push((
                        Box::leak(format!("hair {name} paint {at}").into_boxed_str()),
                        paint.colour[channel],
                    ));
                }
                out.push((at("painted"), paint.density));
            };
            region(
                "scalp",
                hair.scalp.cut,
                hair.scalp.roots,
                hair.scalp.tips,
                hair.scalp.skin,
            );
            region(
                "brows",
                hair.brows.cut,
                hair.brows.roots,
                hair.brows.tips,
                hair.brows.skin,
            );
            region(
                "moustache",
                hair.moustache.cut,
                hair.moustache.roots,
                hair.moustache.tips,
                hair.moustache.skin,
            );
            region(
                "chin",
                hair.chin.cut,
                hair.chin.roots,
                hair.chin.tips,
                hair.chin.skin,
            );
            region(
                "flanks",
                hair.flanks.cut,
                hair.flanks.roots,
                hair.flanks.tips,
                hair.flanks.skin,
            );
            // The closure borrowed `out`; the block it lives in ends here so the
            // pushes below can have it back.
        }
        // Where each region grows, which the panel edits beside what grows there.
        out.push(("hair scalp line", hair.regions.scalp.line));
        out.push(("hair scalp temples", hair.regions.scalp.temples));
        out.push(("hair scalp nape", hair.regions.scalp.nape));
        out.push(("hair brows rise", hair.regions.brows.rise));
        out.push(("hair brows apart", hair.regions.brows.apart));
        out.push(("hair brows reach", hair.regions.brows.reach));
        out.push(("hair brows arch", hair.regions.brows.arch));
        out.push(("hair moustache width", hair.regions.moustache.width));
        out.push(("hair moustache drop", hair.regions.moustache.drop));
        out.push(("hair chin width", hair.regions.chin.width));
        out.push(("hair chin under", hair.regions.chin.under));
        out.push(("hair chin rise", hair.regions.chin.rise));
        out.push(("hair flanks cheek", hair.regions.flanks.cheek));
        out.push(("hair flanks under", hair.regions.flanks.under));
        out.push(("hair flanks sideburn", hair.regions.flanks.sideburn));
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
        // **One entry, and it used to be two.** `hair.locks` was the other, and
        // it went with the sculpted shell whose rim it cut into (symbios-avatar
        // #202): a head of hair is five regions of cards now and nothing about
        // it is a whole number a person sets.
        vec![("age", record.composites.age)]
    }

    /// The hair half of [`fiddled`], which is two thirds of the record's axes.
    ///
    /// Its own function because five regions of fourteen numbers plus fifteen
    /// region shapes is longer than the whole of the rest of the record put
    /// together — and because a list this long is the one most worth reading on
    /// its own when the coverage count moves.
    fn fiddle_hair(record: &mut AvatarRecord) {
        // Every region, every axis, and nowhere a thousandth lands. The five
        // are given DIFFERENT numbers rather than one value each, because a
        // panel that wrote a scalp's cut into a chin's would pass a test that
        // fiddled them all the same way.
        //
        // Negative on the signed ones, because the half of an axis the panel
        // could not reach is exactly the half a test that never set them could
        // not catch (#9) — and the region shapes below are all signed.
        for (at, tress) in [0.717_17_f32, 0.282_82, 0.454_54, 0.616_16, 0.838_38]
            .into_iter()
            .zip([
                &mut record.hair.scalp.cut,
                &mut record.hair.brows.cut,
                &mut record.hair.moustache.cut,
                &mut record.hair.chin.cut,
                &mut record.hair.flanks.cut,
            ])
        {
            tress.length = at;
            tress.thickness = at * 0.7 + 0.070_70;
            tress.density = at * 0.5 + 0.131_31;
            tress.droop = at * 0.3 + 0.212_12;
        }
        for (at, tress) in [0.070_70_f32, 0.191_91, 0.313_13, 0.434_34, 0.555_55]
            .into_iter()
            .zip([
                (
                    &mut record.hair.scalp.roots,
                    &mut record.hair.scalp.tips,
                    &mut record.hair.scalp.skin,
                ),
                (
                    &mut record.hair.brows.roots,
                    &mut record.hair.brows.tips,
                    &mut record.hair.brows.skin,
                ),
                (
                    &mut record.hair.moustache.roots,
                    &mut record.hair.moustache.tips,
                    &mut record.hair.moustache.skin,
                ),
                (
                    &mut record.hair.chin.roots,
                    &mut record.hair.chin.tips,
                    &mut record.hair.chin.skin,
                ),
                (
                    &mut record.hair.flanks.roots,
                    &mut record.hair.flanks.tips,
                    &mut record.hair.flanks.skin,
                ),
            ])
        {
            let (roots, tips, paint) = tress;
            *roots = [at, at + 0.010_10, at + 0.020_20];
            *tips = [at + 0.030_30, at + 0.040_40, at + 0.050_50];
            paint.density = at + 0.060_60;
            paint.colour = [at + 0.070_70, at + 0.080_80, at + 0.090_90];
        }
        record.hair.regions.scalp.line = -0.282_82;
        record.hair.regions.scalp.temples = 0.454_54;
        record.hair.regions.scalp.nape = -0.616_16;
        record.hair.regions.brows.rise = -0.121_21;
        record.hair.regions.brows.apart = 0.232_32;
        record.hair.regions.brows.reach = -0.343_43;
        record.hair.regions.brows.arch = 0.454_54;
        record.hair.regions.moustache.width = -0.565_65;
        record.hair.regions.moustache.drop = 0.676_76;
        record.hair.regions.chin.width = -0.787_87;
        record.hair.regions.chin.under = 0.898_98;
        record.hair.regions.chin.rise = -0.909_09;
        record.hair.regions.flanks.cheek = 0.101_01;
        record.hair.regions.flanks.under = -0.212_12;
        record.hair.regions.flanks.sideburn = 0.323_23;
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
            params.chest_volume = 0.234_56;
            params.chest_projection = -0.876_54;
            params.chest_lift = 0.432_10;
            params.chest_spacing = -0.567_89;
            params.chest_fullness = 0.678_90;
        }
        record.composites.femininity = 0.371_53;
        record.composites.mass = -0.628_47;
        record.composites.body_fat = 0.317_29;
        record.composites.age = 53;
        record.skin.melanin = 0.456_78;
        record.skin.undertone = -0.333_33;
        record.skin.blush = 0.777_77;
        record.skin.freckles = 0.123_45;
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
        fiddle_hair(&mut record);
        record.outfit.top.colour = [0.151_51, 0.626_26, 0.373_73];
        record.outfit.top.length = 0.848_48;
        record.outfit.trousers.colour = [0.919_19, 0.050_50, 0.444_44];
        record.outfit.trousers.length = 0.666_66;
        record.outfit.top.texture = Some(GarmentTexture {
            surface: SurfaceConfig::named("Fabric").expect("Fabric is a surface"),
            scale: 3.333_33,
            rotation: 12.345_67,
        });
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
        //
        // **38 → 116, and 2 → 1** (symbios-avatar #202/#209). Hair stopped being
        // eight scalars describing one sculpted shell and became five follicle
        // regions of two layers each: four cut axes, three colours of three
        // channels, a paint density and the region's own shape axes, times five.
        // Seven axes left and seventy-eight arrived. The whole-number count fell
        // with `hair.locks`, which cut the rim of the shell into wedges and has
        // nothing to cut any more.
        //
        // **116 -> 115** (symbios-avatar #212): `skin.stubble` came off the
        // record. It is the removal direction again, and the third time this
        // guard has caught one — the axis had been dead since the painted hair
        // layer replaced what it drew, and the slider that wrote it went on
        // being drawn.
        //
        // **115 -> 118** (symbios-avatar #273): the chest's three. The
        // addition direction, and the guard caught it the way it is meant to —
        // the sliders were written and this said so before anything shipped
        // with a record axis no panel could reach.
        //
        // **118 -> 120** (symbios-avatar #289): `chestSpacing` and
        // `chestFullness`, milestone #9's last two. The addition direction
        // again, and it is the standing arrangement rather than a catch this
        // time — every engine record-schema change carries an editor slice by
        // default, so the count and the sliders moved together.
        //
        // **120 -> 126** (symbios-avatar #354): the outfit's four hue and
        // shade axes became two garments of an sRGB colour and a length, eight
        // axes, and a garment wearing a texture carries two more — its tiles
        // and its turn — which `fiddled` gives the top.
        let record = fiddled();
        let listed = axes(&record).len();
        assert_eq!(
            listed, 126,
            "the panel's coverage list names {listed} axes; if a record field \
             was added or removed, add it to `axes` and `fiddled` and correct \
             this count"
        );
        assert_eq!(counts(&record).len(), 1, "and the same for whole numbers");
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

    /// The wire names a region's two picker rows offer, cards then helmets:
    /// what the engine's `NAMES` lists. A macro because the styles are five
    /// types and this crate names serde only through `serde_json`.
    macro_rules! offered {
        ($cards:expr, $helmets:expr) => {
            $cards
                .iter()
                .chain(&$helmets)
                .map(|(_, style)| {
                    serde_json::to_value(style).expect("a style serialises")["name"]
                        .as_str()
                        .expect("a style is tagged by name")
                        .to_owned()
                })
                .collect::<Vec<String>>()
        };
    }

    #[test]
    fn every_style_the_engine_names_has_a_picker_in_its_order() {
        // #48, and the standing rule that every engine schema change carries an
        // editor slice. Engine 0.9 added seven helmet scalp styles and a
        // sculpted style to each facial region; a name missing here is a style
        // nobody can pick, which reads exactly like one that does not exist.
        // Held to the engine's own NAMES - every name it writes, in declaration
        // order - so the next name it adds fails this until a row offers it.
        use symbios_avatar::{BrowStyle, ChinStyle, FlankStyle, MoustacheStyle, ScalpStyle};
        let expect = |names: &[&str]| names.iter().map(|n| (*n).to_owned()).collect::<Vec<_>>();
        assert_eq!(
            offered!(SCALP_CARDS, SCALP_HELMETS),
            expect(ScalpStyle::NAMES)
        );
        assert_eq!(offered!(BROW_CARDS, BROW_HELMETS), expect(BrowStyle::NAMES));
        assert_eq!(
            offered!(MOUSTACHE_CARDS, MOUSTACHE_HELMETS),
            expect(MoustacheStyle::NAMES)
        );
        assert_eq!(offered!(CHIN_CARDS, CHIN_HELMETS), expect(ChinStyle::NAMES));
        assert_eq!(
            offered!(FLANK_CARDS, FLANK_HELMETS),
            expect(FlankStyle::NAMES)
        );
    }

    /// The labels one frame of the record's sections shows, read through
    /// AccessKit - what a person using the panel is offered.
    fn offered(record: &mut AvatarRecord) -> Vec<String> {
        labels(|ui| {
            skin_axes(ui, record);
            coat_axes(ui, record);
            fur_axes(ui, record);
            eye_axes(ui, record);
            face_axes(ui, record);
            hair_axes(ui, record);
            outfit_axes(ui, record);
        })
    }

    /// The text one frame of `draw` shows, read through AccessKit.
    fn labels(draw: impl FnMut(&mut egui::Ui)) -> Vec<String> {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        let output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(screen),
                ..Default::default()
            },
            draw,
        );
        let update = output
            .platform_output
            .accesskit_update
            .expect("AccessKit is on");
        update
            .nodes
            .iter()
            // A button's text is its label; a plain label's is its value.
            .filter_map(|(_, node)| node.label().or_else(|| node.value()).map(str::to_owned))
            .collect()
    }

    #[test]
    fn a_mane_s_stiffness_slider_rewrites_nothing_on_sight_and_moves_when_dragged() {
        // symbios-avatar #389. An egui slider with a clamp and a step rewrites
        // what it is shown, with no input, if the value is off its grid: every
        // thousandth a record can hold is drawn idle and must come back as it
        // went in. The default among them must also stay off the wire.
        let mut record =
            AvatarRecord::new("Beast", Archetype::Quadruped(QuadrupedParams::default()));
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 900.0));
        let mut changed = false;
        let run = |events: Vec<egui::Event>, record: &mut AvatarRecord, changed: &mut bool| {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(screen),
                    events,
                    ..Default::default()
                },
                |ui| {
                    *changed = fur_axes(ui, record);
                },
            )
        };
        let bounds_of =
            |output: egui::FullOutput, label: &str, role: Option<egui::accesskit::Role>| {
                let update = output
                    .platform_output
                    .accesskit_update
                    .expect("AccessKit is on");
                update
                    .nodes
                    .iter()
                    .find(|(_, node)| {
                        node.label() == Some(label) && role.is_none_or(|role| node.role() == role)
                    })
                    .and_then(|(_, node)| node.bounds())
                    .unwrap_or_else(|| panic!("fixture: no {label}"))
            };
        // Open the section, as a person does.
        let header = bounds_of(run(Vec::new(), &mut record, &mut changed), "fur", None);
        let at = centre(
            f64::midpoint(header.x0, header.x1),
            f64::midpoint(header.y0, header.y1),
        );
        run(
            vec![egui::Event::PointerMoved(at), press(at, true)],
            &mut record,
            &mut changed,
        );
        run(vec![press(at, false)], &mut record, &mut changed);
        for _ in 0..30 {
            run(Vec::new(), &mut record, &mut changed);
        }
        let stiffness = |record: &AvatarRecord| {
            let Archetype::Quadruped(params) = &record.archetype else {
                panic!("a beast");
            };
            params.fur.mane_stiffness
        };
        let rail = bounds_of(
            run(Vec::new(), &mut record, &mut changed),
            "mane stiffness",
            Some(egui::accesskit::Role::Slider),
        );
        for thousandth in 0..=1000u16 {
            let Archetype::Quadruped(params) = &mut record.archetype else {
                panic!("a beast");
            };
            params.fur.mane_stiffness = f32::from(thousandth) / 1000.0;
            record.sanitize();
            let before = stiffness(&record).to_bits();
            run(Vec::new(), &mut record, &mut changed);
            assert_eq!(
                stiffness(&record).to_bits(),
                before,
                "{thousandth} thousandths rewritten on sight"
            );
            assert!(!changed, "{thousandth} thousandths reported as changed");
        }
        // And a person's drag does reach the record: at a quarter of its rail,
        // well under where the sweep left it.
        let at = centre(
            rail.x0 + (rail.x1 - rail.x0) * 0.25,
            f64::midpoint(rail.y0, rail.y1),
        );
        run(
            vec![egui::Event::PointerMoved(at), press(at, true)],
            &mut record,
            &mut changed,
        );
        assert!(changed, "the drag reported no change");
        assert!(
            stiffness(&record) < 0.5,
            "the drag left {}",
            stiffness(&record)
        );
    }

    #[test]
    fn a_quadruped_is_offered_no_face_hair_or_outfit() {
        // symbios-avatar #365: nothing on a beast reads those axes, and a
        // slider that moves nothing is worse than none. Its eyes are its own.
        let mut beast =
            AvatarRecord::new("Beast", Archetype::Quadruped(QuadrupedParams::default()));
        let mut person = AvatarRecord::new("Person", Archetype::default());
        let (beast, person) = (offered(&mut beast), offered(&mut person));
        for section in ["face", "hair", "outfit"] {
            assert!(
                !beast.iter().any(|label| label == section),
                "{section} was offered for a beast: {beast:?}"
            );
            assert!(
                person.iter().any(|label| label == section),
                "{section} was not offered for a person: {person:?}"
            );
        }
        assert!(
            beast.iter().any(|label| label == "eyes"),
            "a beast's eyes were not offered: {beast:?}"
        );
        // A beast wears a coat and fur where a person wears a complexion
        // (symbios-avatar #368, #369), each offered only where it is worn.
        for (section, beastly) in [("coat", true), ("fur", true), ("skin", false)] {
            assert_eq!(
                beast.iter().any(|label| label == section),
                beastly,
                "{section} on a beast: {beast:?}"
            );
            assert_eq!(
                person.iter().any(|label| label == section),
                !beastly,
                "{section} on a person: {person:?}"
            );
        }
    }

    #[test]
    fn a_quadruped_is_told_why_and_offered_no_lock_on_what_it_never_rolls() {
        // symbios-avatar #365: a re-roll draws no hair and no outfit for a beast,
        // so a lock on either would hold nothing; and the sections hidden further
        // down are named where the body is chosen.
        let mut beast =
            AvatarRecord::new("Beast", Archetype::Quadruped(QuadrupedParams::default()));
        let mut person = AvatarRecord::new("Person", Archetype::default());
        let beast = labels(|ui| {
            identity(ui, &mut beast);
        });
        let person = labels(|ui| {
            identity(ui, &mut person);
        });
        for lock in ["hair", "outfit"] {
            assert!(
                !beast.iter().any(|label| label == lock),
                "a beast was offered a {lock} lock: {beast:?}"
            );
            assert!(
                person.iter().any(|label| label == lock),
                "a person was not offered a {lock} lock: {person:?}"
            );
        }
        // And a beast's fur is rolled under the hair lock, named for what it
        // keeps there (symbios-avatar #369).
        assert!(beast.iter().any(|label| label == "fur"), "{beast:?}");
        assert!(!person.iter().any(|label| label == "fur"), "{person:?}");
        let note = "This body has no face, hair or outfit yet: those are a person's. \
                    Its fur has a section and a lock of its own.";
        assert!(beast.iter().any(|label| label == note), "{beast:?}");
        assert!(!person.iter().any(|label| label == note), "{person:?}");
    }

    /// Click the topmost button labelled `label` that `draw` shows, found
    /// through AccessKit on a first frame, over the frames a click takes.
    fn click(mut draw: impl FnMut(&mut egui::Ui), label: &str) {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 900.0));
        let mut run = |events: Vec<egui::Event>| {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(screen),
                    events,
                    ..Default::default()
                },
                &mut draw,
            )
        };
        let update = run(Vec::new())
            .platform_output
            .accesskit_update
            .expect("AccessKit is on");
        let bounds = update
            .nodes
            .iter()
            .filter(|(_, node)| node.label() == Some(label))
            .filter_map(|(_, node)| node.bounds())
            .min_by(|a, b| a.y0.total_cmp(&b.y0))
            .unwrap_or_else(|| panic!("fixture: no button labelled {label}"));
        let at = centre(
            f64::midpoint(bounds.x0, bounds.x1),
            f64::midpoint(bounds.y0, bounds.y1),
        );
        run(vec![egui::Event::PointerMoved(at), press(at, true)]);
        run(vec![press(at, false)]);
    }

    #[test]
    fn a_quadruped_s_family_pick_carries_its_character_to_the_new_family() {
        // symbios-avatar #366 and #178: the geometry reads only the axes and
        // the composites, so a pick that wrote the family alone would change
        // nothing on screen. A leggy, heavy dog becomes a leggy, heavy cat -
        // what `set_kind` and `CompositePriors::carry` say - and the blend is
        // offered on a cross alone.
        let dog = QuadrupedParams {
            leg_length: 0.45,
            ..QuadrupedParams::default()
        };
        let mut record = AvatarRecord::new("Beast", Archetype::Quadruped(dog));
        record.composites.mass = 0.6;
        let before = record.composites;
        click(
            |ui| {
                body_axes(ui, &mut record);
            },
            "cat",
        );
        let mut expected = dog;
        expected.set_kind(Family::Cat, None, 0.0);
        assert_eq!(record.archetype, Archetype::Quadruped(expected));
        let mut carried = before;
        dog.composite_priors()
            .carry(&expected.composite_priors(), &mut carried);
        assert_eq!(record.composites, carried);
        // And the contract rather than the call: as many spreads from the
        // cat's centre as it was from the dog's.
        let spreads = |mass: f32, kind: &QuadrupedParams| {
            let (centre, spread) = kind.composite_priors().mass;
            (mass - centre) / spread
        };
        assert!(
            (spreads(record.composites.mass, &expected) - spreads(before.mass, &dog)).abs() < 0.01,
            "a heavy dog is as heavy a cat: {} for {}",
            record.composites.mass,
            before.mass
        );
        let Archetype::Quadruped(cat) = record.archetype else {
            unreachable!("asserted above");
        };
        assert!(
            (cat.leg_length - dog.leg_length).abs() > 0.1,
            "the body moved with the pick"
        );

        let mut plain = AvatarRecord::new("Plain", Archetype::Quadruped(cat));
        let shown = labels(|ui| {
            body_axes(ui, &mut plain);
        });
        assert!(!shown.iter().any(|label| label == "blend"), "{shown:?}");
        let mut crossed = AvatarRecord::new(
            "Crossed",
            Archetype::Quadruped(QuadrupedParams {
                hybrid: Some(Family::Horse),
                blend: 0.5,
                ..cat
            }),
        );
        let shown = labels(|ui| {
            body_axes(ui, &mut crossed);
        });
        assert!(shown.iter().any(|label| label == "blend"), "{shown:?}");
    }

    #[test]
    fn a_dragged_blend_lands_where_a_jump_would() {
        // The P1 critic's case: a dog crossed with a horse, dragged along its
        // blend a few thousandths a frame. Re-kinded from each frame's body,
        // the rounding summed over the drag - it landed centimetres from where
        // a jump to the same blend lands; from the body the drag began on, the
        // last frame IS the jump.
        let cross = QuadrupedParams {
            hybrid: Some(Family::Horse),
            blend: 0.25,
            ..QuadrupedParams::default()
        };
        let mut record = AvatarRecord::new("Cross", Archetype::Quadruped(cross));
        record.composites.mass = 0.3;
        record.composites.body_fat = 0.25;
        record.sanitize();
        let origin = record.clone();
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 900.0));
        let run = |events: Vec<egui::Event>, record: &mut AvatarRecord| {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(screen),
                    events,
                    ..Default::default()
                },
                |ui| {
                    body_axes(ui, record);
                },
            )
        };
        let update = run(Vec::new(), &mut record)
            .platform_output
            .accesskit_update
            .expect("AccessKit is on");
        let rail = update
            .nodes
            .iter()
            .find(|(_, node)| {
                node.role() == egui::accesskit::Role::Slider && node.label() == Some("blend")
            })
            .and_then(|(_, node)| node.bounds())
            .expect("fixture: the blend slider is shown on a cross");
        let at = |share: f64| {
            centre(
                rail.x0 + (rail.x1 - rail.x0) * share,
                f64::midpoint(rail.y0, rail.y1),
            )
        };
        let start = at(0.25);
        run(
            vec![egui::Event::PointerMoved(start), press(start, true)],
            &mut record,
        );
        for step in 1..=200 {
            let share = 0.25 + 0.5 * f64::from(step) / 200.0;
            run(vec![egui::Event::PointerMoved(at(share))], &mut record);
        }
        run(vec![press(at(0.75), false)], &mut record);
        run(Vec::new(), &mut record);

        let Archetype::Quadruped(landed) = record.archetype else {
            panic!("archetype changed");
        };
        assert!(
            landed.blend > 0.6,
            "fixture: the drag moved the blend to {}",
            landed.blend
        );
        let Archetype::Quadruped(mut jumped) = origin.archetype else {
            unreachable!("built as one");
        };
        let mut composites = origin.composites;
        let before = jumped.composite_priors();
        jumped.set_kind(Family::Dog, Some(Family::Horse), landed.blend);
        before.carry(&jumped.composite_priors(), &mut composites);
        assert_eq!(landed, jumped, "the drag drifted from the jump");
        assert_eq!(record.composites, composites, "the composites drifted");
        // And the drag did move the body: half a horse further is taller.
        assert!(
            landed.height - cross.height > 0.1,
            "the body did not follow the drag: {} from {}",
            landed.height,
            cross.height
        );
    }

    #[test]
    fn a_quadruped_is_told_it_has_no_frame_axis_and_offered_none() {
        // symbios-avatar #178: `femininity` is a person's axis and nothing on a
        // beast reads it, so the slider would move nothing.
        let mut beast =
            AvatarRecord::new("Beast", Archetype::Quadruped(QuadrupedParams::default()));
        let mut person = AvatarRecord::new("Person", Archetype::default());
        let beast = labels(|ui| {
            composite_axes(ui, &mut beast);
        });
        let person = labels(|ui| {
            composite_axes(ui, &mut person);
        });
        let note = "This body has no frame axis: femininity is a person's.";
        assert!(beast.iter().any(|label| label == note), "{beast:?}");
        // And its frame lock is named for what it keeps there (#366).
        let mut beast_record =
            AvatarRecord::new("Beast", Archetype::Quadruped(QuadrupedParams::default()));
        let mut person_record = AvatarRecord::new("Person", Archetype::default());
        let beast_locks = labels(|ui| {
            identity(ui, &mut beast_record);
        });
        let person_locks = labels(|ui| {
            identity(ui, &mut person_record);
        });
        assert!(
            beast_locks.iter().any(|label| label == "family"),
            "{beast_locks:?}"
        );
        assert!(
            !beast_locks.iter().any(|label| label == "frame"),
            "{beast_locks:?}"
        );
        assert!(
            person_locks.iter().any(|label| label == "frame"),
            "{person_locks:?}"
        );
        assert!(!person.iter().any(|label| label == note), "{person:?}");
        let offered = |shown: &[String]| {
            shown
                .iter()
                .any(|label| label.contains("femininity") && label != note)
        };
        assert!(!offered(&beast), "{beast:?}");
        assert!(offered(&person), "{person:?}");
    }

    #[test]
    fn a_quadruped_with_every_axis_set_survives_the_wire_exactly() {
        // `every_axis_the_panel_writes_survives_the_wire_exactly` for the other
        // plan, a cross with every axis off its neutral (symbios-avatar #366).
        let mut beast = AvatarRecord::new(
            "Beast",
            Archetype::Quadruped(QuadrupedParams {
                height: 1.234_56,
                body_length: -0.456_78,
                leg_length: 0.234_56,
                neck_length: 0.678_91,
                head_size: -0.123_45,
                tail_length: 0.912_34,
                family: Family::Horse,
                hybrid: Some(Family::Dog),
                blend: 0.345_67,
                muzzle_length: 0.456_78,
                stance: 0.876_54,
                neck_carriage: 0.543_21,
                tail_carriage: -0.765_43,
                chest_depth: 0.987_65,
                appeal: 1.123_45,
                muzzle_breadth: -0.345_67,
                stop: 0.654_32,
                eye_set: -0.789_12,
                pupil: Pupil::Slit,
                ear_size: 0.234_56,
                ear_point: -0.567_89,
                ear_carriage: -0.912_34,
                nose: Nose::Soft,
                feet: Feet::Pads,
                whiskers: 0.456_78,
                horn: Horn::Unicorn,
                // symbios-avatar #368: every locus off its default.
                coat: Coat {
                    pigment: Pigment::Liver,
                    dilute: true,
                    agouti: Agouti::TanPoints,
                    pattern: Pattern::Rosettes,
                    points: true,
                    white: White::Irish,
                    merle: true,
                    roan: true,
                    dun: true,
                    tortie: true,
                    intensity: 1.345_67,
                    fantasy: Fantasy::Ocean,
                    marks: 54_321,
                },
                // symbios-avatar #369: every zone off none.
                fur: Fur {
                    mane: 0.987_65,
                    ruff: 1.234_56,
                    tail: 2.345_67,
                    feathering: 0.456_78,
                    tufts: 0.678_91,
                    mane_stiffness: 0.789_12,
                },
            }),
        );
        beast.sanitize();
        let mut editor = RecordEditor::new(beast);
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
        assert_eq!(
            editor.record, back,
            "the record did not survive its own JSON"
        );
    }

    #[test]
    fn throwing_the_marks_again_visits_every_layout_before_one_repeats() {
        let mut seen = vec![false; 1 << 16];
        let mut marks = 0u16;
        for _ in 0..1 << 16 {
            assert!(!seen[usize::from(marks)], "marks {marks} came round early");
            seen[usize::from(marks)] = true;
            marks = next_marks(marks);
        }
        assert_eq!(marks, 0, "and the cycle closes");
    }

    /// Drive [`identity`] for `frames` frames with the input `input` makes of
    /// the frame's number and the centre of the seed's text field (found on
    /// the first frame through AccessKit, by the seed it shows), returning
    /// whether any frame noted the record.
    fn type_into_seed(
        record: &mut AvatarRecord,
        frames: usize,
        input: impl Fn(usize, Fields) -> Vec<egui::Event>,
    ) -> bool {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        let mut field = None;
        let mut noted_any = false;
        for frame in 0..frames {
            let events = field.map(|at| input(frame, at)).unwrap_or_default();
            let shown = record.seed.to_string();
            let named = record.name.clone();
            let output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(screen),
                    events,
                    ..Default::default()
                },
                |ui| {
                    let (_, noted) = identity(ui, record);
                    noted_any |= noted;
                },
            );
            if field.is_none() {
                let update = output
                    .platform_output
                    .accesskit_update
                    .expect("AccessKit is on");
                let text_input = |value: &str| {
                    update
                        .nodes
                        .iter()
                        .find(|(_, node)| {
                            node.role() == egui::accesskit::Role::TextInput
                                && node.value() == Some(value)
                        })
                        .and_then(|(_, node)| node.bounds())
                        .map(|b| centre(f64::midpoint(b.x0, b.x1), f64::midpoint(b.y0, b.y1)))
                };
                field = text_input(&shown)
                    .zip(text_input(&named))
                    .map(|(seed, name)| Fields { seed, name });
            }
        }
        assert!(
            field.is_some(),
            "fixture: the seed and name fields were found"
        );
        noted_any
    }

    /// Where `type_into_seed` found the two text fields of `identity`.
    #[derive(Clone, Copy)]
    struct Fields {
        seed: egui::Pos2,
        name: egui::Pos2,
    }

    /// AccessKit's `f64` point as egui's `f32` one.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "a point on an 800 x 600 test screen fits an f32 exactly enough to click"
    )]
    fn centre(x: f64, y: f64) -> egui::Pos2 {
        egui::pos2(x as f32, y as f32)
    }

    fn press(pos: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        }
    }

    fn key(key: egui::Key, pressed: bool, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers,
        }
    }

    /// overlands #1569: the identity seed holds every `i64`. Typed past 2^53
    /// and applied with Enter, it is written to the last digit and the
    /// record is noted - the host's cue to mark it dirty and record an undo.
    /// A seed past 2^53 clicked into and out of is kept to the last digit,
    /// and nothing is noted; a `DragValue`, which carries an `f64`, wrote it
    /// back rounded and said nothing.
    #[test]
    fn the_identity_seed_holds_every_i64() {
        let exact: i64 = (1 << 53) + 1;
        let mut typed = AvatarRecord::new("Typed", Archetype::default());
        let noted = type_into_seed(&mut typed, 6, |frame, at| match frame {
            1 => vec![egui::Event::PointerMoved(at.seed), press(at.seed, true)],
            2 => vec![press(at.seed, false)],
            3 => vec![
                key(egui::Key::A, true, egui::Modifiers::COMMAND),
                key(egui::Key::A, false, egui::Modifiers::COMMAND),
                egui::Event::Text(exact.to_string()),
                key(egui::Key::Enter, true, egui::Modifiers::NONE),
                key(egui::Key::Enter, false, egui::Modifiers::NONE),
            ],
            _ => Vec::new(),
        });
        assert_eq!(typed.seed, exact);
        assert!(noted, "a typed seed is noted");

        let mut kept = AvatarRecord::new("Kept", Archetype::default());
        kept.seed = i64::MIN + 1;
        let empty = egui::pos2(700.0, 500.0);
        let noted = type_into_seed(&mut kept, 7, |frame, at| match frame {
            1 => vec![egui::Event::PointerMoved(at.seed), press(at.seed, true)],
            2 => vec![press(at.seed, false)],
            4 => vec![egui::Event::PointerMoved(empty), press(empty, true)],
            5 => vec![press(empty, false)],
            _ => Vec::new(),
        });
        assert_eq!(kept.seed, i64::MIN + 1);
        assert!(!noted, "a click in and out is no edit");
    }

    /// A typed seed is applied when focus moves on to a widget drawn ABOVE
    /// the field - the name box - as it is on Enter, Tab or a click on empty
    /// space; Escape is the one way out that drops it. That click reached the
    /// name box before the seed field was drawn, so the field found itself
    /// unfocused, put the old value back into its text, and then applied
    /// that (the Group 7 critic's probe: seed 7, "123" typed, the name box
    /// clicked - the seed stayed 7 and nothing was noted).
    #[test]
    fn a_typed_seed_is_applied_when_focus_moves_up_to_the_name() {
        let typing = |at: Fields| {
            vec![
                key(egui::Key::A, true, egui::Modifiers::COMMAND),
                key(egui::Key::A, false, egui::Modifiers::COMMAND),
                egui::Event::Text(String::from("123")),
                egui::Event::PointerMoved(at.seed),
            ]
        };
        let mut moved = AvatarRecord::new("Moved", Archetype::default());
        moved.seed = 7;
        let noted = type_into_seed(&mut moved, 8, |frame, at| match frame {
            1 => vec![egui::Event::PointerMoved(at.seed), press(at.seed, true)],
            2 => vec![press(at.seed, false)],
            3 => typing(at),
            4 => vec![egui::Event::PointerMoved(at.name), press(at.name, true)],
            5 => vec![press(at.name, false)],
            _ => Vec::new(),
        });
        assert_eq!(moved.seed, 123, "the draft is applied as focus leaves");
        assert!(noted, "and noted");

        let mut escaped = AvatarRecord::new("Escaped", Archetype::default());
        escaped.seed = 7;
        let noted = type_into_seed(&mut escaped, 7, |frame, at| match frame {
            1 => vec![egui::Event::PointerMoved(at.seed), press(at.seed, true)],
            2 => vec![press(at.seed, false)],
            3 => typing(at),
            4 => vec![
                key(egui::Key::Escape, true, egui::Modifiers::NONE),
                key(egui::Key::Escape, false, egui::Modifiers::NONE),
            ],
            _ => Vec::new(),
        });
        assert_eq!(escaped.seed, 7, "Escape drops the draft");
        assert!(!noted, "and notes nothing");
    }

    /// Every label and value a headless pass of `draw` hands AccessKit.
    ///
    /// Two passes, because a context's first frame can be a sizing pass; and
    /// values as well as labels, because egui reports some widgets' shown text
    /// as a value (a `ComboBox`'s selection, #1337).
    fn drawn_text(mut draw: impl FnMut(&mut egui::Ui)) -> Vec<String> {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut text = Vec::new();
        for _ in 0..2 {
            let output = ctx.run_ui(egui::RawInput::default(), &mut draw);
            text = output
                .platform_output
                .accesskit_update
                .map(|update| {
                    update
                        .nodes
                        .iter()
                        .flat_map(|(_, node)| [node.label(), node.value()])
                        .flatten()
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default();
        }
        text
    }

    #[test]
    fn each_picked_style_draws_the_slider_for_its_own_axis_and_no_other() {
        // The match arms in the region pickers are the one place a new
        // variant's axis becomes reachable, and nothing else checks that they
        // are bound: a helmet whose arm returned `false` would compile, pass
        // the wire tests, and put its axis out of reach. Drawn headless and read
        // back through AccessKit, one style at a time, with every OTHER axis
        // label as the control that the reading can tell them apart.
        use symbios_avatar::{ChinStyle, FlankStyle, MoustacheStyle, ScalpStyle};
        let scalp: Vec<(ScalpStyle, Option<&str>)> = SCALP_CARDS
            .iter()
            .chain(&SCALP_HELMETS)
            .map(|(_, style)| {
                let axis = match style {
                    ScalpStyle::None | ScalpStyle::Crop => None,
                    ScalpStyle::Bob { .. } => Some("fringe"),
                    ScalpStyle::Long { .. } => Some("back weight"),
                    ScalpStyle::TiedBack { .. } => Some("tail height"),
                    ScalpStyle::Curly { .. } => Some("curl"),
                    ScalpStyle::Cap { .. } => Some("fringe notch"),
                    ScalpStyle::SlickBack { .. } => Some("volume"),
                    ScalpStyle::Bell { .. } => Some("hem length"),
                    ScalpStyle::Bun { .. } => Some("bun height"),
                    ScalpStyle::Crest { .. } => Some("crest height"),
                    ScalpStyle::Afro { .. } => Some("size"),
                    ScalpStyle::Braids { .. } => Some("rows"),
                };
                (*style, axis)
            })
            .collect();
        let every: Vec<&str> = scalp.iter().filter_map(|(_, axis)| *axis).collect();
        for (style, axis) in &scalp {
            let mut drawn = *style;
            let text = drawn_text(|ui| {
                scalp_style(ui, &mut drawn);
            });
            for name in &every {
                assert_eq!(
                    text.iter().any(|t| t == name),
                    Some(*name) == *axis,
                    "{style:?} drew the {name} slider: {}, with {text:?}",
                    text.iter().any(|t| t == name)
                );
            }
            // And the style is marked picked: its row's label is on screen.
            assert!(text.iter().any(|t| t == "helmet") && text.iter().any(|t| t == "cards"));
        }

        // The facial regions' one new axis each, and a sculpted brow and
        // flanks draw no slider at all.
        let mut moustache = MoustacheStyle::Sculpted { flare: 0.3 };
        let text = drawn_text(|ui| {
            moustache_style(ui, &mut moustache);
        });
        assert!(text.iter().any(|t| t == "flare"), "{text:?}");
        let mut chin = ChinStyle::Sculpted { length: 0.3 };
        let text = drawn_text(|ui| {
            chin_style(ui, &mut chin);
        });
        assert!(text.iter().any(|t| t == "hang"), "{text:?}");
        let mut flanks = FlankStyle::Sculpted;
        let text = drawn_text(|ui| {
            flank_style(ui, &mut flanks);
        });
        assert!(
            !text.iter().any(|t| t == "drop" || t == "reach"),
            "a sculpted flank drew a card style's slider: {text:?}"
        );
    }

    #[test]
    fn a_style_off_the_middle_of_its_axis_is_still_the_one_marked_picked() {
        // The rows used to compare whole values, so a bob at fringe 0.8 matched
        // no label: nothing was marked, and a click on "bob" reset the fringe to
        // the row's 0.5. By variant, the bob is the bob wherever its axis is.
        let mut style = symbios_avatar::ScalpStyle::Bob { fringe: 0.8 };
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut selected = Vec::new();
        for _ in 0..2 {
            let output = ctx.run_ui(egui::RawInput::default(), |ui| {
                scalp_style(ui, &mut style);
            });
            selected = output
                .platform_output
                .accesskit_update
                .map(|update| {
                    update
                        .nodes
                        .iter()
                        // egui reports a selectable label's state as toggled.
                        .filter(|(_, node)| node.toggled() == Some(egui::accesskit::Toggled::True))
                        .filter_map(|(_, node)| node.label().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
        }
        assert_eq!(selected, ["bob"], "marked picked: {selected:?}");
        assert_eq!(
            style,
            symbios_avatar::ScalpStyle::Bob { fringe: 0.8 },
            "drawing moved the axis"
        );
    }

    #[test]
    fn a_style_kept_from_a_newer_build_is_said_on_the_panel_while_the_region_is_none() {
        // Engine 0.9 keeps a style name it does not know and writes it back
        // while the region stays none. The panel shows that region as none, so
        // it has to say what is being kept - painted, since a hover explains
        // nothing to somebody who does not know to hover. Control: the same
        // record once a style is picked over it says nothing, because from then
        // on the kept style is not written.
        let hair: symbios_avatar::HairRecord =
            serde_json::from_str(r#"{"scalp":{"style":{"name":"mohawk_2031","spikes":800}}}"#)
                .expect("an unknown style name loads since engine 0.9");
        assert!(
            hair.unrecognised.scalp.is_some(),
            "the engine did not keep it"
        );
        let mut record = AvatarRecord::new("Kept", Archetype::default());
        record.hair = hair;

        let said = |record: &AvatarRecord| {
            let kept = record.hair.unrecognised.scalp.clone();
            let none = record.hair.scalp.style == symbios_avatar::ScalpStyle::None;
            drawn_text(|ui| kept_unknown(ui, none, kept.as_ref()))
                .iter()
                .any(|t| t.contains("mohawk_2031"))
        };
        assert!(said(&record), "a kept style went unmentioned");
        record.hair.scalp.style = symbios_avatar::ScalpStyle::Crop;
        assert!(
            !said(&record),
            "the line stayed after a style was picked over it"
        );
    }

    /// A headless app running the real rebuild against the real compute pool.
    fn building_app() -> App {
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            AssetPlugin::default(),
            bevy::mesh::MeshPlugin,
            bevy::image::ImagePlugin::default(),
        ))
        .init_asset::<StandardMaterial>()
        .init_asset::<SkinnedMeshInverseBindposes>()
        .init_resource::<RecordEditor>()
        .add_systems(Update, rebuild_edited_avatar);
        app
    }

    /// How long to wait on the whole ladder before calling it a hang.
    ///
    /// Wall clock, not a tick count, and the distinction is the bug this test
    /// had. What it waits for is two real avatar builds on the compute pool,
    /// which take seconds; a tick count measures how fast an almost-empty
    /// schedule spins, which has nothing to do with them. Worse, the two move
    /// in opposite directions — a loaded machine makes each tick slower and so
    /// hands the guard MORE real time, so the old count passed under load and
    /// failed on an idle box.
    ///
    /// Sized for the slowest machine this is expected to run on rather than for
    /// this one: a two-core CI runner is far from the box a developer sees, and
    /// a stall guard that trips on a slow runner is a flake, not a test. Here it
    /// finishes in about two seconds.
    const LADDER_DEADLINE: Duration = Duration::from_mins(2);

    #[test]
    fn nothing_settles_until_the_full_body_is_the_one_on_screen() {
        // A capture that counts frames from startup photographs a scene that is
        // still arriving. Frames are not the unit — a body lands off the compute
        // pool when it lands — so this walks the real pipeline and holds
        // `settled` to its word at every stage of it.
        let mut app = building_app();
        assert!(
            !app.world().resource::<RecordEditor>().settled(),
            "an editor settled before it had built anything at all"
        );

        let full = AvatarConfig::default().atlas;
        let started = Instant::now();
        let mut saw_draft = false;
        loop {
            app.update();
            let editor = app.world().resource::<RecordEditor>();
            let settled = editor.settled();
            let last_build = editor.last_build();
            let bodies = app
                .world_mut()
                .query::<&AvatarBody>()
                .iter(app.world())
                .count();

            match last_build {
                // Nothing has landed yet, or a draft has. Either way the body
                // on screen is not the one a judgement is made from, and this
                // is exactly the window the old frame count could fire in.
                None => assert!(!settled, "settled with no build behind it"),
                Some((_, atlas)) if atlas != full => {
                    saw_draft = true;
                    assert!(!settled, "settled on a {atlas} draft atlas");
                }
                Some(_) => {
                    assert!(settled, "the full body landed and nothing settled");
                    assert_eq!(bodies, 1, "settled without a body in the world");
                    break;
                }
            }
            // The draft, the 250 ms settle and the full build are all real work
            // on a real pool; this is a stall guard, not a schedule. It reports
            // the rung it stopped on, because "no full body" alone cannot tell a
            // build that hung from a record that never built at all.
            assert!(
                started.elapsed() < LADDER_DEADLINE,
                "no full body after {:?}, stuck on {}",
                started.elapsed(),
                match last_build {
                    None => String::from("no build at all"),
                    Some((took, atlas)) => format!("a {atlas} build that took {took:?}"),
                }
            );
        }
        // Not incidental — it is the whole reason `settled` cannot just ask
        // whether a body exists. If the ladder ever stops going up through a
        // draft, this assertion is the thing that says so.
        assert!(saw_draft, "the draft rung of the ladder never appeared");
    }

    #[test]
    fn a_draft_standing_in_is_never_settled() {
        // The one rung the walk above cannot be counted on to land on, and the
        // only one where `settled`'s `draft` term is what answers.
        //
        // A draft stands in with nothing on the pool for the stretch between it
        // landing and the settle clock reaching `SETTLE` — in the viewer about
        // 180 ms, and exactly the window a capture can fire in. Whether the walk
        // ever observes it depends on whether the draft build outran the settle,
        // which is a fact about the machine and not about this crate: on the box
        // this was written on it never does, so every frame the walk sees at the
        // draft rung is held unsettled by `building` instead. That leaves the
        // term that matters resting on a coincidence of timing. Set the flags
        // directly and it rests on nothing.
        let mut editor = RecordEditor {
            dirty: false,
            building: false,
            draft: true,
            last_build: Some((Duration::from_millis(68), DRAFT_ATLAS)),
            ..Default::default()
        };
        assert!(!editor.settled(), "settled while a draft stood in");

        // The same editor, one full build later. Only `draft` moved, so only
        // `draft` can be what changed the answer.
        editor.draft = false;
        editor.last_build = Some((Duration::from_millis(277), AvatarConfig::default().atlas));
        assert!(editor.settled(), "the full body landed and nothing settled");
    }

    #[test]
    fn every_surface_crosses_to_the_texture_panel_and_back_unchanged() {
        // The texture panel is `bevy_symbios_texture`'s, and it edits that
        // crate's own tagged config; a garment holds the engine's. The bridge
        // between them is their shared native JSON, and a surface it dropped a
        // field of, or renamed, would be edited by a panel that is not showing
        // the garment's config.
        for surface in SurfaceConfig::all() {
            let config = texture_config(&surface)
                .unwrap_or_else(|| panic!("{} did not reach the panel", surface.name()));
            assert_eq!(
                config.label(),
                surface.name(),
                "the panel shows another surface"
            );
            let back = surface_config(&config)
                .unwrap_or_else(|| panic!("{} did not come back", surface.name()));
            assert_eq!(back, surface, "{} changed across the panel", surface.name());
        }
        // One this build cannot draw has no panel, rather than a wrong one.
        let mut unknown = serde_json::Map::new();
        unknown.insert("$type".into(), serde_json::Value::from("Tartan"));
        assert!(texture_config(&SurfaceConfig::Unknown(unknown)).is_none());
    }
}
