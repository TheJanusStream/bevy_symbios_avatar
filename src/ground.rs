//! The ground under one body, as data a component can carry (#56).
//!
//! The engine asks what lies beneath a point through a closure, so that the
//! same foot placement runs against a physics engine, a heightmap or a flat
//! plane in a test. A closure is a borrow of whatever it reads, and a
//! component cannot hold a borrow - which is why
//! [`drive_avatar_bodies`](crate::drive_avatar_bodies) gave every body a level
//! floor until now. [`GroundPatch`] is the shape that crosses that line: the
//! ground under one body sampled into a small square of heights in the body's
//! own frame, written by whatever knows the world and answered from inside the
//! driver.
//!
//! # Why a grid in the body's frame
//!
//! **The body's frame because that is the frame the engine asks in.** Every
//! point the driver hands its ground closure is in the frame the body is posed
//! in, the rig root's, and the answer has to come back in it. A patch sampled
//! there needs no transform at all when it is read, so nothing about where the
//! body stands in the world can get out of step between the writer and the
//! driver: the writer does that mapping once, while sampling.
//!
//! **A grid because a foot asks about a neighbourhood, not a point.** Within one
//! frame the stride seats each contact, the plant probes under each foot twice
//! and the toes lay themselves on the normal under them, all at points nobody
//! outside the driver knows in advance. So the patch covers everywhere a foot
//! can reach - [`GroundPatch::reach`] sizes that from the rig - and answers any
//! point inside it bilinearly.
//!
//! # Using it
//!
//! ```no_run
//! use bevy::prelude::*;
//! use bevy_symbios_avatar::{AvatarBody, Drive, GroundPatch};
//!
//! /// A world whose ground rises a metre for every five travelled down `+Z`.
//! fn ground_y(world: Vec3) -> f32 {
//!     world.z * 0.2
//! }
//!
//! /// Reads each body's `Transform`, not its `GlobalTransform`: see below.
//! fn stand_on_the_hill(mut bodies: Query<(&Transform, &AvatarBody, &mut Drive)>) {
//!     for (root, body, mut drive) in &mut bodies {
//!         let Some(reach) = GroundPatch::reach(&body.avatar.rig) else {
//!             drive.ground = None;
//!             continue;
//!         };
//!         // Reuse last frame's allocation: this runs every frame.
//!         let mut patch = drive.ground.take().unwrap_or_default();
//!         patch.resample(reach, 0.25, |at| {
//!             let world = root.transform_point(Vec3::new(at.x, 0.0, at.y));
//!             ground_y(world) - root.translation.y
//!         });
//!         drive.ground = Some(patch);
//!     }
//! }
//! ```
//!
//! **Sample through the pose the body is drawn at this frame.** In `Update` an
//! entity's `GlobalTransform` is still the one the last frame's `PostUpdate`
//! propagated, while the body is drawn from the `Transform` this frame leaves,
//! so a patch sampled through it is the ground where the body was a frame ago:
//! over a slope break at a gallop that puts the feet centimetres into the
//! ground or over it for several frames (#56). The example's bodies are
//! top-level, so their `Transform` is their pose in the world; a body hung
//! under a moving parent composes the parent's current pose with its own the
//! same way.

use bevy::math::Rect;
use bevy::prelude::*;
use symbios_avatar::Rig;
use symbios_avatar::anim::Ground;
use symbios_avatar::anim::driver::level_ground;

/// The most samples a patch keeps along a side.
///
/// A body's reach at the finest step anyone has a use for is a few dozen: a
/// horse's legs reach about a metre past its stance on every side, so at a
/// quarter metre its patch is 16 samples a side, a cat's 6. The bound is for the caller who asks for a
/// millimetre step over a field, which would otherwise allocate without limit:
/// past it the step is widened until the area fits.
pub const MAX_SIDE: usize = 128;

/// How far a sample may stand from the root's own level, in metres.
///
/// Ten kilometres, which no ground under a body's feet is. Heights are clamped
/// to it as they are written, so a caller's sentinel - a `f32::MAX` meaning
/// "nothing here" - cannot overflow the interpolation and come back as an
/// infinite surface or a `NaN` normal.
pub const HEIGHT_LIMIT: f32 = 10_000.0;

/// The ground under one body: a square of heights in the body's own frame,
/// answered bilinearly (#56).
///
/// **The body's own frame** is the frame it is posed in - the rig root's, its
/// forward `+Z` and its left `+X` - which is the frame
/// [`Driver::drive`](symbios_avatar::anim::Driver::drive) asks its ground in.
/// Heights are measured from the root's `y = 0`, the level floor
/// [`level_ground`] answers, so a patch of zeros is exactly that floor.
///
/// The samples lie on a square grid: `side` of them along `+X` and as many
/// along `+Z`, `step` metres apart, the first at `origin` (the grid's smallest
/// `x` and `z`). They are stored row by row, so the sample at column `c` and
/// row `r` - at `origin + (c, r) * step` - is `heights[r * side + c]`.
///
/// # What it answers
///
/// [`Self::ground`] answers any point with the surface the samples span: the
/// height bilinearly interpolated between the four around it, and the normal
/// of that interpolated surface, from its gradient. **Past the grid's edge the
/// edge holds**, the height of the nearest edge carried straight out and the
/// surface flat in the direction it was carried, so a foot that reaches past
/// the patch finds the last ground the writer knew rather than a cliff.
///
/// **It is finite and it always answers.** A patch with too few samples to span
/// a surface, a step that is not a positive finite length, or a corner that is
/// not finite answers exactly as [`level_ground`] does. A height that is not
/// finite is written as the root's own level, and every height is clamped to
/// [`HEIGHT_LIMIT`] as it is written, so whatever a caller samples, the driver
/// is never handed a `NaN`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GroundPatch {
    origin: Vec2,
    step: f32,
    side: usize,
    heights: Vec<f32>,
}

impl GroundPatch {
    /// A patch from samples a caller already has.
    ///
    /// `heights` is row by row, `side` by `side` (see the type's own doc). A
    /// patch whose `heights` is not exactly that long is kept as given and
    /// answers as [`level_ground`] does: there is no way to know which samples
    /// a caller meant to leave out.
    #[must_use]
    pub fn new(origin: Vec2, step: f32, side: usize, mut heights: Vec<f32>) -> Self {
        for height in &mut heights {
            *height = written(*height);
        }
        Self {
            origin,
            step,
            side,
            heights,
        }
    }

    /// A patch covering `area` at `step`, each height read from `height` at
    /// its sample's place in the body's frame (`x` and `z`, as a [`Vec2`]).
    ///
    /// See [`Self::resample`], which this is on an empty patch.
    #[must_use]
    pub fn sampled(area: Rect, step: f32, height: impl FnMut(Vec2) -> f32) -> Self {
        let mut patch = Self::default();
        patch.resample(area, step, height);
        patch
    }

    /// Cover `area` at `step` again, reading every height afresh from
    /// `height`, and keep the allocation.
    ///
    /// **For a writer that runs every frame**, which is every writer: a body
    /// that moves needs its ground re-read every frame it moves. The grid is
    /// the square centred on `area` with sides at least as long as its longer
    /// one, so `area` is covered whichever way it is longer, and its step is
    /// `step` unless that would take more than [`MAX_SIDE`] samples a side, in
    /// which case it is widened until the square fits.
    ///
    /// An `area` whose corners or whose size are not finite - corners at
    /// `-f32::MAX` and `f32::MAX` are finite and span an infinite square - or a
    /// `step` that is not a positive finite length, leaves an empty patch,
    /// which answers as [`level_ground`] does.
    #[expect(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a side is at most MAX_SIDE, and a grid coordinate is clamped into 0..side first"
    )]
    pub fn resample(&mut self, area: Rect, step: f32, mut height: impl FnMut(Vec2) -> f32) {
        self.heights.clear();
        let span = area.size().max_element();
        // The span as well as the corners: two finite corners far enough
        // apart overflow it, and an infinite span made an infinite step and
        // MAX_SIDE squared samples at points that are not finite (#56).
        if !(area.min.is_finite()
            && area.max.is_finite()
            && span.is_finite()
            && step.is_finite()
            && step > 0.0)
        {
            self.side = 0;
            return;
        }
        // Whole steps across the longer side, and one sample more than steps.
        // A point area still takes a 2 by 2 grid, so it spans a surface.
        let wanted = (span / step).ceil() + 1.0;
        let (side, step) = if wanted > MAX_SIDE as f32 {
            (MAX_SIDE, span / (MAX_SIDE - 1) as f32)
        } else {
            ((wanted as usize).max(2), step)
        };
        let reach = (side - 1) as f32 * step;
        self.origin = area.center() - Vec2::splat(reach * 0.5);
        self.step = step;
        self.side = side;
        self.heights.reserve(side * side);
        for row in 0..side {
            for column in 0..side {
                let at = self.sample_at(column, row);
                self.heights.push(written(height(at)));
            }
        }
    }

    /// The part of a body's own floor its feet can reach, sized from its rig:
    /// the rest stance - every ground contact's sole, across and along - grown
    /// on every side by the longest of those legs' reach
    /// ([`Rig::limb_reach`]).
    ///
    /// **Read from the rig rather than guessed**, because the bodies a consumer
    /// carries run from a cat to a horse: a margin that suits one leaves the
    /// other's feet off the edge of its patch, or samples ground no foot can
    /// touch, every frame. A foot cannot be further from its hip than its leg
    /// reaches, and a hip is above its stance, so a planted foot, a swinging
    /// one and the point a stride anticipates all fall inside this.
    ///
    /// [`None`] for a body that stands on nothing.
    #[must_use]
    pub fn reach(rig: &Rig) -> Option<Rect> {
        let stance = Self::stance(rig)?;
        let reach = rig
            .ground_contacts()
            .into_iter()
            .filter_map(|limb| rig.limb_reach(limb))
            .fold(0.0f32, f32::max);
        Some(stance.inflate(reach))
    }

    /// Where a body's soles are at rest, across (`x`) and along (`z`) its own
    /// floor: the smallest rectangle holding every extremity joint of every
    /// limb it stands on ([`Rig::ground_contacts`]).
    ///
    /// The footprint a consumer asks about when it wants to know whether a
    /// body is standing on a surface, rather than where its feet might go.
    /// [`None`] for a body that stands on nothing.
    #[must_use]
    pub fn stance(rig: &Rig) -> Option<Rect> {
        rig.ground_contacts()
            .into_iter()
            .flat_map(|limb| rig.extremity_joints(limb))
            .map(|joint| {
                let at = rig.joints[joint].position;
                Vec2::new(at.x, at.z)
            })
            .fold(None, |held: Option<Rect>, at| {
                Some(held.map_or(Rect::from_corners(at, at), |held| held.union_point(at)))
            })
    }

    /// The grid's first sample - its smallest `x` and `z` - in the body's
    /// frame.
    #[must_use]
    pub fn origin(&self) -> Vec2 {
        self.origin
    }

    /// Metres between neighbouring samples, along either axis.
    #[must_use]
    pub fn step(&self) -> f32 {
        self.step
    }

    /// Samples along each side.
    #[must_use]
    pub fn side(&self) -> usize {
        self.side
    }

    /// Every height, row by row: the sample at column `c` and row `r` is
    /// `heights()[r * side() + c]`.
    #[must_use]
    pub fn heights(&self) -> &[f32] {
        &self.heights
    }

    /// Where the sample at `column` along `+X` and `row` along `+Z` lies, in
    /// the body's frame.
    #[must_use]
    #[expect(clippy::cast_precision_loss, reason = "a grid's indices are small")]
    pub fn sample_at(&self, column: usize, row: usize) -> Vec2 {
        self.origin + Vec2::new(column as f32, row as f32) * self.step
    }

    /// Whether the patch spans a surface at all. One that does not answers as
    /// [`level_ground`] does.
    #[must_use]
    pub fn spans(&self) -> bool {
        self.side >= 2
            && self.side.checked_mul(self.side) == Some(self.heights.len())
            && self.step.is_finite()
            && self.step > 0.0
            && self.origin.is_finite()
    }

    /// What lies beneath `point`, in the body's frame: the closure
    /// [`Driver::drive`](symbios_avatar::anim::Driver::drive) takes, as
    /// `|point| patch.ground(point)`.
    ///
    /// The position is `point`'s own `x` and `z` on the interpolated surface,
    /// and the normal is that surface's; past the grid's edge the edge holds.
    /// **Always answers**, and with exactly what [`level_ground`] answers when
    /// the patch spans no surface or `point` is not finite across: a body on a
    /// patch of zeros is driven bit for bit as one on the level floor.
    #[must_use]
    #[expect(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a side is at most MAX_SIDE, and a grid coordinate is clamped into 0..side first"
    )]
    pub fn ground(&self, point: Vec3) -> Option<Ground> {
        if !(self.spans() && point.x.is_finite() && point.z.is_finite()) {
            return level_ground(point);
        }
        let last = (self.side - 1) as f32;
        let along = |at: f32, from: f32| {
            let grid = (at - from) / self.step;
            // Which cell, and how far across it: the last cell owns the far
            // edge, so a point there interpolates rather than indexing past it.
            let cell = grid.clamp(0.0, last).floor().min(last - 1.0);
            let across = grid.clamp(0.0, last) - cell;
            // Inside the grid the surface slopes; carried past an edge it is
            // flat in the direction it was carried.
            let inside = (0.0..=last).contains(&grid);
            (cell as usize, across, inside)
        };
        let (column, x, inside_x) = along(point.x, self.origin.x);
        let (row, z, inside_z) = along(point.z, self.origin.y);
        let at = |c: usize, r: usize| self.heights[r * self.side + c];
        let (h00, h10) = (at(column, row), at(column + 1, row));
        let (h01, h11) = (at(column, row + 1), at(column + 1, row + 1));
        let near = h00 + (h10 - h00) * x;
        let far = h01 + (h11 - h01) * x;
        let height = near + (far - near) * z;
        // The bilinear surface's own gradient at the point, per metre.
        let rise_x = if inside_x {
            ((h10 - h00) + ((h11 - h01) - (h10 - h00)) * z) / self.step
        } else {
            0.0
        };
        let rise_z = if inside_z {
            (far - near) / self.step
        } else {
            0.0
        };
        // Exactly `+Y` on flat ground, rather than the `-0.0` components a
        // negated zero gradient would carry into every rotation built on it:
        // a patch of zeros must drive bit for bit as the level floor does.
        let normal = if rise_x == 0.0 && rise_z == 0.0 {
            Vec3::Y
        } else {
            Vec3::new(-rise_x, 1.0, -rise_z).normalize_or(Vec3::Y)
        };
        Some(Ground {
            position: Vec3::new(point.x, height, point.z),
            normal,
        })
    }
}

/// A height as a patch keeps it: finite, and within [`HEIGHT_LIMIT`].
fn written(height: f32) -> f32 {
    if height.is_finite() {
        height.clamp(-HEIGHT_LIMIT, HEIGHT_LIMIT)
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A plane rising `grade` metres per metre toward the body's front and
    /// `camber` toward its left, sampled over a two-metre square.
    fn plane(grade: f32, camber: f32) -> GroundPatch {
        GroundPatch::sampled(Rect::new(-1.0, -1.0, 1.0, 1.0), 0.25, |at| {
            at.x * camber + at.y * grade
        })
    }

    #[test]
    fn a_plane_is_answered_exactly_inside_and_held_at_its_edge_outside() {
        let patch = plane(0.2, -0.1);
        assert_eq!(patch.side(), 9);
        let normal = Vec3::new(0.1, 1.0, -0.2).normalize();
        for (x, z) in [(0.0, 0.0), (0.3, -0.7), (-1.0, 1.0), (0.99, 0.01)] {
            let ground = patch.ground(Vec3::new(x, 5.0, z)).expect("always answers");
            assert!(
                (ground.position - Vec3::new(x, z * 0.2 - x * 0.1, z)).length() < 1e-5,
                "({x}, {z}): {:?}",
                ground.position
            );
            assert!(
                (ground.normal - normal).length() < 1e-5,
                "{:?}",
                ground.normal
            );
        }
        // Two metres past the front edge: the edge's height, and the surface
        // flat along the way it was carried but still cambered across.
        let past = patch.ground(Vec3::new(0.5, 0.0, 3.0)).expect("answers");
        assert!((past.position.y - (0.2 - 0.05)).abs() < 1e-5, "{past:?}");
        assert!(
            (past.normal - Vec3::new(0.1, 1.0, 0.0).normalize()).length() < 1e-5,
            "{past:?}"
        );
    }

    #[test]
    fn a_patch_of_zeros_answers_bit_for_bit_as_the_level_floor() {
        let patch = plane(0.0, 0.0);
        for point in [
            Vec3::ZERO,
            Vec3::new(0.37, 1.2, -0.81),
            Vec3::new(-4.0, -2.0, 9.0),
            Vec3::new(-0.0, 0.0, -0.0),
        ] {
            let (ours, level) = (patch.ground(point), level_ground(point));
            let bits = |g: Option<Ground>| {
                g.map(|g| {
                    (
                        g.position.to_array().map(f32::to_bits),
                        g.normal.to_array().map(f32::to_bits),
                    )
                })
            };
            assert_eq!(bits(ours), bits(level), "{point}");
        }
    }

    #[test]
    fn a_degenerate_patch_answers_as_the_level_floor() {
        let point = Vec3::new(0.2, 1.0, -0.3);
        let level = level_ground(point);
        for patch in [
            GroundPatch::default(),
            GroundPatch::new(Vec2::ZERO, 0.25, 1, vec![3.0]),
            GroundPatch::new(Vec2::ZERO, 0.25, 3, vec![3.0; 8]),
            GroundPatch::new(Vec2::ZERO, 0.0, 2, vec![3.0; 4]),
            GroundPatch::new(Vec2::ZERO, f32::NAN, 2, vec![3.0; 4]),
            GroundPatch::new(Vec2::NAN, 0.25, 2, vec![3.0; 4]),
            GroundPatch::new(Vec2::ZERO, 0.25, usize::MAX, vec![3.0; 4]),
            GroundPatch::sampled(Rect::new(0.0, 0.0, f32::INFINITY, 1.0), 0.25, |_| 3.0),
            GroundPatch::sampled(Rect::new(-f32::MAX, 0.0, f32::MAX, 1.0), 0.25, |_| 3.0),
            GroundPatch::sampled(Rect::new(0.0, 0.0, 1.0, 1.0), -1.0, |_| 3.0),
        ] {
            assert!(!patch.spans(), "{patch:?}");
            assert_eq!(patch.ground(point), level, "{patch:?}");
        }
    }

    /// Finite corners far enough apart span an infinite square (#56): that
    /// area is not finite, and nothing is sampled for it, where it once took
    /// an infinite step and asked for `MAX_SIDE` squared heights at points
    /// that were not finite.
    #[test]
    fn an_area_of_infinite_size_samples_nothing() {
        let mut patch = plane(0.2, 0.0);
        let mut asked = 0usize;
        patch.resample(Rect::new(-f32::MAX, -1.0, f32::MAX, 1.0), 0.25, |at| {
            asked += 1;
            at.y
        });
        assert_eq!(asked, 0, "sampled an area that is not finite");
        assert_eq!((patch.side(), patch.heights().len()), (0, 0));
        assert!(!patch.spans());
    }

    #[test]
    fn whatever_is_sampled_the_answer_is_finite() {
        let patch = GroundPatch::sampled(Rect::new(-1.0, -1.0, 1.0, 1.0), 0.5, |at| {
            match (at.x > 0.0, at.y > 0.0) {
                (true, true) => f32::NAN,
                (true, false) => f32::MAX,
                (false, true) => f32::NEG_INFINITY,
                (false, false) => -f32::MAX,
            }
        });
        for x in [-2.0, -0.6, -0.1, 0.0, 0.3, 0.9, 4.0] {
            for z in [-2.0, -0.4, 0.0, 0.2, 0.8, 3.0] {
                let ground = patch.ground(Vec3::new(x, 0.0, z)).expect("answers");
                assert!(
                    ground.position.is_finite() && ground.normal.is_finite(),
                    "{ground:?}"
                );
                assert!((ground.normal.length() - 1.0).abs() < 1e-4, "{ground:?}");
                assert!(ground.position.y.abs() <= HEIGHT_LIMIT);
            }
        }
        // A point not finite across is the level floor's to answer, NaN and
        // all, rather than an index computed from it.
        let nan = Vec3::new(f32::NAN, 0.0, 0.0);
        assert!(patch.ground(nan).expect("answers").position.x.is_nan());
    }

    #[test]
    #[expect(clippy::cast_precision_loss, reason = "a grid's sides are small")]
    fn a_resample_covers_its_area_and_a_huge_one_is_bounded() {
        let mut patch = plane(0.2, 0.0);
        let area = Rect::new(-0.4, 0.1, 0.6, 2.3);
        patch.resample(area, 0.25, |at| at.y);
        let reach = (patch.side() - 1) as f32 * patch.step();
        assert!(patch.origin().x <= area.min.x && patch.origin().y <= area.min.y);
        assert!(patch.origin().x + reach >= area.max.x && patch.origin().y + reach >= area.max.y);
        assert_eq!(patch.heights().len(), patch.side() * patch.side());

        patch.resample(Rect::new(-500.0, -500.0, 500.0, 500.0), 0.001, |_| 0.0);
        assert_eq!(patch.side(), MAX_SIDE);
        assert!((patch.step() - 1000.0 / (MAX_SIDE - 1) as f32).abs() < 1e-3);
    }
}
