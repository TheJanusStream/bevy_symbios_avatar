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
//!     changed |= editor::body_axes(ui, &mut record.archetype);
//!     changed |= editor::skin_axes(ui, record);
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
use symbios_avatar::{
    Archetype, Avatar, AvatarConfig, AvatarRecord, Category, GENERATOR_VERSION, HumanoidParams,
    Leg, Limb, QuadrupedParams, Rig, Sleeve, Zone,
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

    ui.horizontal(|ui| {
        ui.label("seed");
        // Typing a seed records which draw a body came from; it does not
        // re-draw one. That is the button beside it.
        noted |= ui
            .add(egui::DragValue::new(&mut record.seed).speed(1.0))
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
    // exactly, but pressing either seed arrow redraws it under today's rules
    // and it will not come back the same person.
    if record.generator != GENERATOR_VERSION {
        ui.horizontal_wrapped(|ui| {
            ui.small(format!(
                "⚠ rolled by generator {}, this build draws {} — the stored axes are \
                 exact, but re-rolling this seed will not reproduce it",
                record.generator, GENERATOR_VERSION
            ));
        });
    }

    ui.horizontal_wrapped(|ui| {
        ui.label("locked");
        for category in Category::ALL {
            let mut locked = record.locks.is_locked(category);
            if ui
                .toggle_value(&mut locked, category_name(category))
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
            let names: Vec<&str> = held.into_iter().map(category_name).collect();
            ui.small(format!("hunting, holding {}", names.join(", ")));
        }
    }
    (changed, noted)
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
pub fn body_axes(ui: &mut egui::Ui, archetype: &mut Archetype) -> bool {
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
pub fn skin_axes(ui: &mut egui::Ui, record: &mut AvatarRecord) -> bool {
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
pub fn face_axes(ui: &mut egui::Ui, record: &mut AvatarRecord) -> bool {
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
pub fn hair_axes(ui: &mut egui::Ui, record: &mut AvatarRecord) -> bool {
    let mut changed = false;
    egui::CollapsingHeader::new("hair").show(ui, |ui| {
        changed |= zone(ui, "scalp", |ui| {
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
            let mut changed = moustache_style(ui, &mut record.hair.moustache.style);
            changed |= tress(ui, &mut record.hair.moustache);
            ui.separator();
            changed |= signed(ui, "width", &mut record.hair.regions.moustache.width);
            changed |= signed(ui, "drop", &mut record.hair.regions.moustache.drop);
            changed
        });
        changed |= zone(ui, "chin", |ui| {
            let mut changed = chin_style(ui, &mut record.hair.chin.style);
            changed |= tress(ui, &mut record.hair.chin);
            ui.separator();
            changed |= signed(ui, "width", &mut record.hair.regions.chin.width);
            changed |= signed(ui, "under", &mut record.hair.regions.chin.under);
            changed |= signed(ui, "rise", &mut record.hair.regions.chin.rise);
            changed
        });
        changed |= zone(ui, "flanks", |ui| {
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
        for stop in 0..=8u32 {
            let shade = f32::from(u16::try_from(stop).unwrap_or(0)) / 8.0;
            let tone = ramp(shade);
            let picked = tone
                .iter()
                .zip(colour.iter())
                .all(|(a, b)| (a - b).abs() < 0.004);
            let button = egui::Button::new(if picked { "•" } else { " " })
                .fill(swatch(symbios_avatar::Vec3::from_array(tone)))
                .min_size(egui::vec2(18.0, 18.0));
            if ui
                .add(button)
                .on_hover_text(format!("{hover} {shade:.2}"))
                .clicked()
            {
                *colour = tone;
                changed = true;
            }
        }
        let mut free = swatch(symbios_avatar::Vec3::from_array(*colour));
        if ui
            .color_edit_button_srgba(&mut free)
            .on_hover_text("any colour, sRGB")
            .changed()
        {
            *colour = [
                f32::from(free.r()) / 255.0,
                f32::from(free.g()) / 255.0,
                f32::from(free.b()) / 255.0,
            ];
            changed = true;
        }
    });
    changed
}

/// A row of base styles, one selectable label each.
///
/// Picking a style hands back a fresh instance of it rather than trying to carry
/// an axis across: a bob's fringe and a tail's height are different quantities
/// that happen to be spelled the same way, and carrying one into the other is
/// how a panel writes a haircut nobody asked for.
fn styles<S: Copy + PartialEq>(ui: &mut egui::Ui, current: &mut S, choices: &[(&str, S)]) -> bool {
    let mut changed = false;
    ui.horizontal_wrapped(|ui| {
        for (name, style) in choices {
            let picked = current == style;
            if ui.selectable_label(picked, *name).clicked() && !picked {
                *current = *style;
                changed = true;
            }
        }
    });
    changed
}

/// The scalp's catalogue, and whichever axis the picked style carries.
fn scalp_style(ui: &mut egui::Ui, style: &mut symbios_avatar::ScalpStyle) -> bool {
    use symbios_avatar::ScalpStyle as S;
    let mut changed = styles(
        ui,
        style,
        &[
            ("none", S::None),
            ("crop", S::Crop),
            ("bob", S::Bob { fringe: 0.5 }),
            ("long", S::Long { weight: 0.5 }),
            ("tied", S::TiedBack { tail: 0.5 }),
            ("curly", S::Curly { curl: 0.5 }),
        ],
    );
    // Matched rather than looked up, because the axis's NAME is part of the
    // style: "fringe" and "tail" mean different things and a panel that called
    // both of them "axis" would be a panel nobody can use.
    changed |= match style {
        S::None | S::Crop => false,
        S::Bob { fringe } => axis(ui, "fringe", fringe, 0.0..=1.0),
        S::Long { weight } => axis(ui, "back weight", weight, 0.0..=1.0),
        S::TiedBack { tail } => axis(ui, "tail height", tail, 0.0..=1.0),
        S::Curly { curl } => axis(ui, "curl", curl, 0.0..=1.0),
    };
    changed
}

/// The brows' catalogue, which carries no axis of its own.
fn brow_style(ui: &mut egui::Ui, style: &mut symbios_avatar::BrowStyle) -> bool {
    use symbios_avatar::BrowStyle as S;
    styles(
        ui,
        style,
        &[
            ("none", S::None),
            ("natural", S::Natural),
            ("thick", S::Thick),
        ],
    )
}

/// The upper lip's catalogue.
fn moustache_style(ui: &mut egui::Ui, style: &mut symbios_avatar::MoustacheStyle) -> bool {
    use symbios_avatar::MoustacheStyle as S;
    let mut changed = styles(
        ui,
        style,
        &[
            ("none", S::None),
            ("chevron", S::Chevron),
            ("handlebar", S::Handlebar { sweep: 0.5 }),
            ("pencil", S::Pencil { ride: 0.5 }),
        ],
    );
    changed |= match style {
        S::None | S::Chevron => false,
        S::Handlebar { sweep } => axis(ui, "sweep", sweep, 0.0..=1.0),
        S::Pencil { ride } => axis(ui, "ride", ride, 0.0..=1.0),
    };
    changed
}

/// The chin's catalogue.
fn chin_style(ui: &mut egui::Ui, style: &mut symbios_avatar::ChinStyle) -> bool {
    use symbios_avatar::ChinStyle as S;
    let mut changed = styles(
        ui,
        style,
        &[
            ("none", S::None),
            ("goatee", S::Goatee { point: 0.5 }),
            ("full", S::Full),
            ("braided", S::Braided { twist: 0.5 }),
        ],
    );
    changed |= match style {
        S::None | S::Full => false,
        S::Goatee { point } => axis(ui, "point", point, 0.0..=1.0),
        S::Braided { twist } => axis(ui, "twist", twist, 0.0..=1.0),
    };
    changed
}

/// The jaw flanks' catalogue.
fn flank_style(ui: &mut egui::Ui, style: &mut symbios_avatar::FlankStyle) -> bool {
    use symbios_avatar::FlankStyle as S;
    let mut changed = styles(
        ui,
        style,
        &[
            ("none", S::None),
            ("sideburns", S::Sideburns { drop: 0.5 }),
            ("full connect", S::FullConnect { reach: 0.5 }),
        ],
    );
    changed |= match style {
        S::None => false,
        S::Sideburns { drop } => axis(ui, "drop", drop, 0.0..=1.0),
        S::FullConnect { reach } => axis(ui, "reach", reach, 0.0..=1.0),
    };
    changed
}

/// What the body is wearing.
pub fn outfit_axes(ui: &mut egui::Ui, record: &mut AvatarRecord) -> bool {
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
                ("chest_volume", p.chest_volume),
                ("chest_projection", p.chest_projection),
                ("chest_lift", p.chest_lift),
                ("chest_spacing", p.chest_spacing),
                ("chest_fullness", p.chest_fullness),
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
            ("top hue", record.outfit.top_hue),
            ("top shade", record.outfit.top_shade),
            ("leg hue", record.outfit.leg_hue),
            ("leg shade", record.outfit.leg_shade),
        ]);
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
        let record = fiddled();
        let listed = axes(&record).len();
        assert_eq!(
            listed, 120,
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
}
