//! Planning and stitching frame strips across a gait cycle.
//!
//! The strip is the unit of review for symbios-avatar's milestone #11 (Natural
//! walking, its #325): N frames evenly spaced across one gait cycle, rendered
//! from a fixed camera and laid out in a row — and, stacked under it, the same
//! N phases of other renderings of the same body, so a procedural walk and a
//! reference clip can be read frame-against-frame at matched phase. One
//! stitched image rather than N loose files, because the review loop lives on
//! one-glance comparison.
//!
//! This module is the half of the instrument that can be tested without a GPU:
//! which samples to take, in what order, holding what state — and how the
//! captured pixels are laid out into the sheet. The viewer example owns the
//! other half (driving the [`crate::Animator`] through the plan and capturing
//! frames), because that half IS a Bevy app.
//!
//! **Phase-matched by cycle fraction, not wall time.** A gait's cycle and a
//! clip's duration are different lengths of real time; sampling both at
//! `k / frames` of their own cycle is what makes column `k` the same moment of
//! the walk in every row. That decision lives in [`StripPlan::cycle`] and
//! nowhere else.

/// What one row of a strip shows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Row {
    /// The procedural gait, with the head-level bargain on or off.
    ///
    /// `head_level: false` is the ablation row: the neck's counter-rotation is
    /// left off so the head goes down with the trunk, which splits the lean's
    /// contribution to a silhouette from the crane's.
    Gait {
        /// Whether the neck takes the trunk's lean back off.
        head_level: bool,
    },
    /// A baked reference clip, by its index into [`crate::Clips`].
    Clip {
        /// The clip's position in the library.
        index: usize,
        /// Added to every phase of this row, wrapping.
        ///
        /// Phase-matching by cycle fraction makes column `k` the same
        /// FRACTION of each row's cycle, but a clip's frame 0 is whatever
        /// moment its author exported first — nothing makes it the gait's own
        /// zero. This aligns the two origins, found once per clip by eye and
        /// then recorded; without it every column compares two different
        /// moments of the stride and the sheet's one-glance claim is false.
        align: f32,
    },
}

/// One frame to capture: where it lands in the sheet and the state to hold.
///
/// Everything the viewer must write onto the [`crate::Animator`] before the
/// capture is spelled out here rather than recomputed at capture time, so the
/// plan can be tested as data and the capture loop stays a dumb executor.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    /// Which row of the sheet this frame belongs to, top to bottom.
    pub row: usize,
    /// Which column, left to right.
    pub column: usize,
    /// Where in the cycle to hold the body, `0..1`.
    ///
    /// For a clip row this is the fraction of the clip's own duration, which
    /// is how phase-matching works: the animator already runs a clip's time as
    /// `cycle * duration`.
    pub cycle: f32,
    /// The pace to hold for this frame.
    pub pace: f32,
    /// Whether the neck's head-releveling runs.
    pub head_level: bool,
    /// The clip to play instead of the gait, if this is a clip row.
    pub clip: Option<usize>,
}

/// The strip being taken: its shape, and every capture in order.
#[derive(Debug, Clone)]
pub struct StripPlan {
    /// The rows, top to bottom — also the legend.
    pub rows: Vec<Row>,
    /// Frames per row.
    pub frames: usize,
    /// Every capture, row-major: a whole row's columns before the next row.
    ///
    /// Row-major so the animator switches source (gait against clip) once per
    /// row rather than once per frame; every switch is a place the capture
    /// loop can be cheated by a transition, so the plan takes as few as it can.
    pub samples: Vec<Sample>,
}

impl StripPlan {
    /// A steady-cycle strip: every row sampled at phases `k / frames`.
    ///
    /// The phases stop short of `1.0` because a cycle wraps: frame `frames`
    /// would be frame `0` again. Note when judging a clip row near the wrap:
    /// most of the baked loops pause a frame there (the engine's
    /// `docs/clips.md` has the table), and that pause is the source's, not the
    /// gait's.
    ///
    /// # Panics
    ///
    /// On fewer than 2 frames or an empty row set — a strip that cannot show
    /// motion is a strip nothing can be judged on, and the caller validated
    /// its flags before building the plan.
    #[must_use]
    pub fn cycle(rows: Vec<Row>, frames: usize, pace: f32) -> Self {
        assert!(frames >= 2, "a strip of {frames} frames cannot show motion");
        assert!(!rows.is_empty(), "a strip with no rows shows nothing");
        let samples = rows
            .iter()
            .enumerate()
            .flat_map(|(at, row)| {
                (0..frames).map(move |k| {
                    #[expect(clippy::cast_precision_loss, reason = "frame counts are tiny")]
                    let phase = k as f32 / frames as f32;
                    let (cycle, head_level, clip) = match *row {
                        Row::Gait { head_level } => (phase, head_level, None),
                        Row::Clip { index, align } => {
                            ((phase + align).rem_euclid(1.0), true, Some(index))
                        }
                    };
                    Sample {
                        row: at,
                        column: k,
                        cycle,
                        pace,
                        head_level,
                        clip,
                    }
                })
            })
            .collect();
        Self {
            rows,
            frames,
            samples,
        }
    }

    /// An acceleration strip: frames across a speed step rather than a steady
    /// cycle.
    ///
    /// The columns are wall-clock samples `span` seconds wide, the cycle
    /// advancing at `cadence` through them, and the pace stepping from `from`
    /// to `to` at the strip's midpoint over `ramp` seconds — because the thing
    /// this strip exists to show is only visible across a change: a term that
    /// follows pace snaps when pace steps, and no steady-state frame can show
    /// a snap. `ramp` defaults in the viewer to the consuming app's measured
    /// chassis profile, which reaches a new speed in about 0.016 s.
    ///
    /// Gait rows only: a baked clip has no pace to step.
    ///
    /// # Panics
    ///
    /// As [`StripPlan::cycle`], and on a non-positive `span` or `cadence` —
    /// zero seconds of wall clock or a cycle that never advances both make
    /// every column the same frame.
    #[must_use]
    pub fn accel(head_levels: &[bool], frames: usize, step: PaceStep, motion: AccelClock) -> Self {
        assert!(frames >= 2, "a strip of {frames} frames cannot show motion");
        assert!(
            !head_levels.is_empty(),
            "a strip with no rows shows nothing"
        );
        assert!(
            motion.span > 0.0 && motion.cadence > 0.0,
            "an accel strip needs wall clock to cross and a cycle that advances"
        );
        let rows: Vec<Row> = head_levels
            .iter()
            .map(|&head_level| Row::Gait { head_level })
            .collect();
        #[expect(clippy::cast_precision_loss, reason = "frame counts are tiny")]
        let dt = motion.span / (frames - 1) as f32;
        let samples = head_levels
            .iter()
            .enumerate()
            .flat_map(|(at, &head_level)| {
                (0..frames).map(move |k| {
                    #[expect(clippy::cast_precision_loss, reason = "frame counts are tiny")]
                    let t = k as f32 * dt;
                    Sample {
                        row: at,
                        column: k,
                        cycle: (t * motion.cadence).fract(),
                        pace: step.at(t - motion.span * 0.5),
                        head_level,
                        clip: None,
                    }
                })
            })
            .collect();
        Self {
            rows,
            frames,
            samples,
        }
    }
}

/// A pace step: `from` before it, `to` after, ramping linearly across `ramp`
/// seconds centred on the step's own zero.
#[derive(Debug, Clone, Copy)]
pub struct PaceStep {
    /// The pace held before the step.
    pub from: f32,
    /// The pace held after it.
    pub to: f32,
    /// How long the change takes, in seconds. Zero snaps.
    pub ramp: f32,
}

impl PaceStep {
    /// The pace `t` seconds from the step's centre (negative is before).
    #[must_use]
    pub fn at(&self, t: f32) -> f32 {
        if self.ramp <= 0.0 {
            return if t < 0.0 { self.from } else { self.to };
        }
        let through = ((t / self.ramp) + 0.5).clamp(0.0, 1.0);
        self.from + (self.to - self.from) * through
    }
}

/// The wall clock an accel strip's columns are spaced along.
#[derive(Debug, Clone, Copy)]
pub struct AccelClock {
    /// How many seconds the whole strip spans.
    pub span: f32,
    /// Cycles per second the gait runs at while crossing it.
    pub cadence: f32,
}

/// One captured frame's pixels, RGBA8 row-major.
#[derive(Debug, Clone)]
pub struct Frame {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// `width * height * 4` bytes.
    pub pixels: Vec<u8>,
}

/// The gap between columns, in pixels.
pub const COLUMN_GUTTER: u32 = 2;
/// The gap between rows, in pixels — wider than the columns' because the
/// judgement is made DOWN a column (this frame against the reference at the
/// same phase), so the row boundary is the one that has to be findable at a
/// glance.
pub const ROW_GUTTER: u32 = 6;
/// What the gutters are filled with: a mid grey that reads as a frame edge on
/// both a dark scene and a pale one.
pub const GUTTER_RGBA: [u8; 4] = [96, 96, 96, 255];

/// Stitches captured frames into one sheet, `columns` per row.
///
/// Frames arrive in the plan's own order (row-major); the sheet is rows of
/// `columns` cells with [`COLUMN_GUTTER`] between columns and [`ROW_GUTTER`]
/// between rows.
///
/// # Panics
///
/// If the frames disagree about their size, are empty, or are not a whole
/// number of rows — any of those means the capture loop lost or duplicated a
/// frame, and a sheet quietly built around that would be an instrument lying
/// about what it photographed.
#[must_use]
pub fn stitch(frames: &[Frame], columns: usize) -> Frame {
    assert!(!frames.is_empty() && columns > 0, "nothing to stitch");
    assert_eq!(
        frames.len() % columns,
        0,
        "{} frames do not fill rows of {columns}",
        frames.len()
    );
    let (w, h) = (frames[0].width, frames[0].height);
    for (at, frame) in frames.iter().enumerate() {
        assert!(
            frame.width == w && frame.height == h,
            "frame {at} is {}x{}, the first was {w}x{h}",
            frame.width,
            frame.height
        );
        assert_eq!(
            frame.pixels.len(),
            (frame.width * frame.height * 4) as usize,
            "frame {at}'s pixel buffer does not match its size"
        );
    }
    let rows = frames.len() / columns;
    #[expect(
        clippy::cast_possible_truncation,
        reason = "sheet dimensions are small"
    )]
    let sheet_w = w * columns as u32 + COLUMN_GUTTER * (columns as u32 - 1);
    #[expect(
        clippy::cast_possible_truncation,
        reason = "sheet dimensions are small"
    )]
    let sheet_h = h * rows as u32 + ROW_GUTTER * (rows as u32 - 1);
    let mut pixels = GUTTER_RGBA.repeat((sheet_w * sheet_h) as usize);
    for (at, frame) in frames.iter().enumerate() {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "sheet dimensions are small"
        )]
        let x0 = (at % columns) as u32 * (w + COLUMN_GUTTER);
        #[expect(
            clippy::cast_possible_truncation,
            reason = "sheet dimensions are small"
        )]
        let y0 = (at / columns) as u32 * (h + ROW_GUTTER);
        for y in 0..h {
            let from = (y * w * 4) as usize;
            let to = (((y0 + y) * sheet_w + x0) * 4) as usize;
            pixels[to..to + (w * 4) as usize]
                .copy_from_slice(&frame.pixels[from..from + (w * 4) as usize]);
        }
    }
    Frame {
        width: sheet_w,
        height: sheet_h,
        pixels,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cycle_strip_samples_every_row_at_the_same_phases() {
        // Phase-matching is the plan's one invariant: column k must be the
        // same moment of the walk in every row, or the sheet compares nothing.
        let plan = StripPlan::cycle(
            vec![
                Row::Gait { head_level: true },
                Row::Gait { head_level: false },
                Row::Clip {
                    index: 3,
                    align: 0.25,
                },
            ],
            8,
            1.4,
        );
        assert_eq!(plan.samples.len(), 24);
        for sample in &plan.samples {
            #[expect(clippy::cast_precision_loss, reason = "frame counts are tiny")]
            let phase = sample.column as f32 / 8.0;
            // A clip row rides the same phases through its own origin: the
            // align offset shifts every column identically, wrapping.
            let expected = if sample.clip.is_some() {
                (phase + 0.25).rem_euclid(1.0)
            } else {
                phase
            };
            assert!((sample.cycle - expected).abs() < 1e-6, "{sample:?}");
            assert!((sample.pace - 1.4).abs() < 1e-6);
        }
        // Row-major: a whole row before the next, so the source switches once
        // per row.
        let rows: Vec<usize> = plan.samples.iter().map(|sample| sample.row).collect();
        assert!(rows.windows(2).all(|pair| pair[0] <= pair[1]));
        // The rows carry their own state: the ablation row alone drops the
        // bargain, the clip row alone names a clip.
        assert!(
            plan.samples[..8]
                .iter()
                .all(|s| s.head_level && s.clip.is_none())
        );
        assert!(plan.samples[8..16].iter().all(|s| !s.head_level));
        assert!(plan.samples[16..].iter().all(|s| s.clip == Some(3)));
    }

    #[test]
    fn phases_stop_short_of_the_wrap() {
        // Frame N would be frame 0 again: a cycle wraps, so the last phase is
        // (N-1)/N and never 1.0.
        let plan = StripPlan::cycle(vec![Row::Gait { head_level: true }], 10, 1.0);
        let last = plan.samples.last().unwrap();
        assert!((last.cycle - 0.9).abs() < 1e-6);
    }

    #[test]
    fn an_accel_strip_holds_from_then_ramps_to_to() {
        // The strip exists to make a snap visible, so the plan must actually
        // contain the change: pure `from` at the left edge, pure `to` at the
        // right, and the crossing at the middle.
        let step = PaceStep {
            from: 1.0,
            to: 1.8,
            ramp: 0.016,
        };
        let clock = AccelClock {
            span: 1.2,
            cadence: 1.1,
        };
        let plan = StripPlan::accel(&[true], 13, step, clock);
        assert!((plan.samples[0].pace - 1.0).abs() < 1e-6);
        assert!((plan.samples[12].pace - 1.8).abs() < 1e-6);
        // Monotone across the step — a ramp, not a wobble.
        let paces: Vec<f32> = plan.samples.iter().map(|sample| sample.pace).collect();
        assert!(paces.windows(2).all(|pair| pair[1] >= pair[0]));
        // And the cycle advances as a clock, not a scrub of one cycle: column
        // spacing times cadence, wrapping.
        let dt = 1.2 / 12.0;
        for sample in &plan.samples {
            #[expect(clippy::cast_precision_loss, reason = "frame counts are tiny")]
            let expected = (sample.column as f32 * dt * 1.1).fract();
            assert!((sample.cycle - expected).abs() < 1e-5, "{sample:?}");
        }
    }

    #[test]
    fn a_zero_ramp_snaps_and_a_ramp_crosses_its_midpoint() {
        let snap = PaceStep {
            from: 0.5,
            to: 1.5,
            ramp: 0.0,
        };
        assert!((snap.at(-1e-6) - 0.5).abs() < 1e-6);
        assert!((snap.at(0.0) - 1.5).abs() < 1e-6);
        let ramp = PaceStep {
            from: 0.5,
            to: 1.5,
            ramp: 0.2,
        };
        assert!((ramp.at(0.0) - 1.0).abs() < 1e-6);
        assert!((ramp.at(-0.1) - 0.5).abs() < 1e-6);
        assert!((ramp.at(0.1) - 1.5).abs() < 1e-6);
    }

    /// A 1-pixel frame of one colour.
    fn dot(rgba: [u8; 4]) -> Frame {
        Frame {
            width: 1,
            height: 1,
            pixels: rgba.to_vec(),
        }
    }

    #[test]
    fn stitching_places_every_frame_where_its_cell_is() {
        // Four 1x1 frames in two columns: the sheet is gutters plus exactly
        // those four pixels, each at its cell's corner.
        let frames = [
            dot([255, 0, 0, 255]),
            dot([0, 255, 0, 255]),
            dot([0, 0, 255, 255]),
            dot([255, 255, 0, 255]),
        ];
        let sheet = stitch(&frames, 2);
        assert_eq!(sheet.width, 1 + COLUMN_GUTTER + 1);
        assert_eq!(sheet.height, 1 + ROW_GUTTER + 1);
        let at = |x: u32, y: u32| {
            let from = ((y * sheet.width + x) * 4) as usize;
            [
                sheet.pixels[from],
                sheet.pixels[from + 1],
                sheet.pixels[from + 2],
                sheet.pixels[from + 3],
            ]
        };
        assert_eq!(at(0, 0), [255, 0, 0, 255]);
        assert_eq!(at(1 + COLUMN_GUTTER, 0), [0, 255, 0, 255]);
        assert_eq!(at(0, 1 + ROW_GUTTER), [0, 0, 255, 255]);
        assert_eq!(at(1 + COLUMN_GUTTER, 1 + ROW_GUTTER), [255, 255, 0, 255]);
        assert_eq!(at(1, 0), GUTTER_RGBA);
    }
}
