//! Keyframe animations (topic 7, work item 6): `enter`, `animation`,
//! and animations in variants.
//!
//! An animation is shared keyframes plus timing. Keyframes are ordered
//! frames, each an offset `at` in [0, 1], some channel values, and an
//! optional easing for the segment that starts at it. A channel a frame
//! leaves out is interpolated between the frames that set it; the
//! implicit frames at 0 and 1 take the underlying value (CSS).
//!
//! While an animation touches a property of a node (it "covers" it),
//! the row holds the animated sample, and the value every other writer
//! declares (a mutation, a variant, a transition's tween) goes to the
//! node's underlying values instead (`Ui::absorb`). Each frame samples
//! the nodes with animations (and only those) over the underlying
//! values, in composite order: enter, then the node's own list, then
//! variants by specificity; the later one wins per channel. When the
//! last animation covering a property ends, the row takes the underlying
//! value back at once.
//!
//! Timing follows Web Animations: a delay, `iterations` of `duration`
//! (fractional or infinite), a direction, and a fill for the phases
//! before and after the active one.

use std::collections::HashMap;
use std::sync::Arc;

use craie_core::rev::Rev;

use crate::animation::{MAX_SECS, Prop, Spring, bezier, end_reason, lerp_color};
use crate::host::{NodeId, Spatial, SpatialPatch};
use crate::states::value_field;
use crate::ui::Ui;

/// How a segment's progress runs from 0 to 1.
#[derive(Clone, Debug, PartialEq)]
pub enum Easing {
    /// CSS `cubic-bezier(x1, y1, x2, y2)`.
    Bezier([f32; 4]),
    /// CSS `steps(n, jump)` (`jump`).
    Steps { n: u32, jump: u8 },
    /// CSS `linear(...)`: (input, output) points, inputs ascending.
    Linear(Arc<[[f32; 2]]>),
    /// A damped spring from rest. As an animation's easing it sets the
    /// duration to its settle time (`settle`, seconds).
    Spring {
        stiffness: f32,
        damping: f32,
        mass: f32,
        settle: f32,
    },
}

/// `steps()` jump positions.
pub mod jump {
    pub const START: u8 = 0;
    pub const END: u8 = 1;
    pub const NONE: u8 = 2;
    pub const BOTH: u8 = 3;
}

impl Easing {
    pub const LINEAR: Easing = Easing::Bezier([0.0, 0.0, 1.0, 1.0]);

    /// A spring easing (its settle time computed once).
    pub fn spring(stiffness: f32, damping: f32, mass: f32) -> Easing {
        let pos = |v: f32| v.is_finite() && v > 0.0 && v <= 1e6;
        let settle = if pos(stiffness) && pos(damping) && pos(mass) {
            Spring::new(stiffness, damping, mass).settle() as f32
        } else {
            f32::NAN
        };
        Easing::Spring {
            stiffness,
            damping,
            mass,
            settle,
        }
    }

    pub fn is_valid(&self) -> bool {
        match self {
            Easing::Bezier([x1, y1, x2, y2]) => {
                (0.0..=1.0).contains(x1)
                    && (0.0..=1.0).contains(x2)
                    && y1.is_finite()
                    && y2.is_finite()
                    && y1.abs() <= 100.0
                    && y2.abs() <= 100.0
            }
            Easing::Steps { n, jump } => {
                *jump <= jump::BOTH && *n > (*jump == jump::NONE) as u32 && *n <= 10_000
            }
            Easing::Linear(points) => {
                (2..=MAX_POINTS).contains(&points.len())
                    && points
                        .iter()
                        .all(|p| p[0].is_finite() && p[1].is_finite() && p[1].abs() <= 100.0)
                    && points.windows(2).all(|w| w[0][0] <= w[1][0])
            }
            Easing::Spring { settle, .. } => (0.0..=MAX_SECS).contains(settle),
        }
    }

    /// Progress at `x` in [0, 1]; may leave [0, 1] (overshoot).
    pub fn at(&self, x: f64) -> f64 {
        self.eval(x, false)
    }

    /// `at`, with CSS's before flag: in a backwards-filled delay, a step
    /// that jumps at `x` has not jumped yet (`steps(n, jump-start)` is 0
    /// there, not 1/n).
    fn eval(&self, x: f64, before: bool) -> f64 {
        match self {
            Easing::Bezier([x1, y1, x2, y2]) => {
                bezier(x, *x1 as f64, *y1 as f64, *x2 as f64, *y2 as f64)
            }
            Easing::Steps { n, jump } => {
                let n = *n as f64;
                let mut step = (x * n).floor();
                if matches!(*jump, jump::START | jump::BOTH) {
                    step += 1.0;
                }
                if before && (x * n).fract() == 0.0 {
                    step -= 1.0;
                }
                let jumps = match *jump {
                    jump::NONE => n - 1.0,
                    jump::BOTH => n + 1.0,
                    _ => n,
                };
                step.clamp(0.0, jumps) / jumps
            }
            Easing::Linear(points) => {
                let (first, last) = (points[0], points[points.len() - 1]);
                if x <= first[0] as f64 {
                    return first[1] as f64;
                }
                if x >= last[0] as f64 {
                    return last[1] as f64;
                }
                let k = points.partition_point(|p| (p[0] as f64) <= x);
                let (a, b) = (points[k - 1], points[k]);
                let span = (b[0] - a[0]) as f64;
                let t = if span > 0.0 {
                    (x - a[0] as f64) / span
                } else {
                    1.0
                };
                a[1] as f64 + (b[1] - a[1]) as f64 * t
            }
            Easing::Spring {
                stiffness,
                damping,
                mass,
                settle,
            } => {
                if x >= 1.0 {
                    return 1.0;
                }
                1.0 - Spring::new(*stiffness, *damping, *mass).offset(x * *settle as f64)
            }
        }
    }
}

/// Most points in a `linear()` easing.
pub const MAX_POINTS: usize = 256;
/// Most frames in one keyframes list.
pub const MAX_FRAMES: usize = 256;
/// Most animations in one list (a node's `enter`, `animation`, or one
/// variant's).
pub const MAX_ANIMATIONS: usize = 16;

/// The channels a frame may set, as `value_field` bits: colors,
/// opacity, and the transform parts per axis.
pub const CHANNELS: u16 = value_field::FILL
    | value_field::BORDER_COLOR
    | value_field::COLOR
    | value_field::OPACITY
    | value_field::TRANSLATE_X
    | value_field::TRANSLATE_Y
    | value_field::ROTATE
    | value_field::SCALE_X
    | value_field::SCALE_Y;

/// The keyframe-animatable values of a node, or one frame's (the
/// channels in its mask).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sample {
    pub fill: u32,
    pub border: u32,
    /// The inherited color. `None` (no color of its own) only as an
    /// underlying value: a frame always sets one.
    pub color: Option<u32>,
    pub opacity: f32,
    /// x, y in points, then x, y as fractions of the border box.
    pub translate: [f32; 4],
    /// Radians.
    pub rotate: f32,
    pub scale: [f32; 2],
}

impl Default for Sample {
    fn default() -> Sample {
        Sample {
            fill: 0,
            border: 0,
            color: None,
            opacity: 1.0,
            translate: [0.0; 4],
            rotate: 0.0,
            scale: [1.0, 1.0],
        }
    }
}

impl Sample {
    /// Copies `prop`'s value from `src`.
    fn copy(&mut self, src: &Sample, prop: Prop) {
        match prop {
            Prop::Fill => self.fill = src.fill,
            Prop::BorderColor => self.border = src.border,
            Prop::Color => self.color = src.color,
            Prop::Opacity => self.opacity = src.opacity,
            Prop::Translate => self.translate = src.translate,
            Prop::Rotate => self.rotate = src.rotate,
            Prop::Scale => self.scale = src.scale,
            _ => {}
        }
    }

    fn get(&self, channel: u16) -> Ch {
        use value_field::*;
        let t = self.translate;
        match channel {
            FILL => Ch::Color(Some(self.fill)),
            BORDER_COLOR => Ch::Color(Some(self.border)),
            COLOR => Ch::Color(self.color),
            OPACITY => Ch::Num([self.opacity, 0.0]),
            TRANSLATE_X => Ch::Num([t[0], t[2]]),
            TRANSLATE_Y => Ch::Num([t[1], t[3]]),
            ROTATE => Ch::Num([self.rotate, 0.0]),
            SCALE_X => Ch::Num([self.scale[0], 0.0]),
            _ => Ch::Num([self.scale[1], 0.0]),
        }
    }

    fn set(&mut self, channel: u16, v: Ch) {
        use value_field::*;
        match (channel, v) {
            (FILL, Ch::Color(Some(c))) => self.fill = c,
            (BORDER_COLOR, Ch::Color(Some(c))) => self.border = c,
            (COLOR, Ch::Color(c)) => self.color = c,
            (OPACITY, Ch::Num([o, _])) => self.opacity = o,
            (TRANSLATE_X, Ch::Num([p, f])) => [self.translate[0], self.translate[2]] = [p, f],
            (TRANSLATE_Y, Ch::Num([p, f])) => [self.translate[1], self.translate[3]] = [p, f],
            (ROTATE, Ch::Num([r, _])) => self.rotate = r,
            (SCALE_X, Ch::Num([s, _])) => self.scale[0] = s,
            (SCALE_Y, Ch::Num([s, _])) => self.scale[1] = s,
            _ => {}
        }
    }

    /// In range: finite, opacity in [0, 1], a color where `mask` has one.
    fn check(&self, mask: u16) -> Result<(), &'static str> {
        if mask & value_field::COLOR != 0 && self.color.is_none() {
            return Err("keyframe color missing");
        }
        if !(self.opacity.is_finite()
            && self.translate.iter().all(|v| v.is_finite())
            && self.rotate.is_finite()
            && self.scale.iter().all(|v| v.is_finite()))
        {
            return Err("keyframe value not finite");
        }
        if !(0.0..=1.0).contains(&self.opacity) {
            return Err("keyframe opacity outside [0, 1]");
        }
        Ok(())
    }
}

/// One channel's value.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Ch {
    /// A number, or a translate axis (points, fraction).
    Num([f32; 2]),
    Color(Option<u32>),
}

fn lerp(a: Ch, b: Ch, p: f32) -> Ch {
    match (a, b) {
        (Ch::Num(a), Ch::Num(b)) => Ch::Num([a[0] + (b[0] - a[0]) * p, a[1] + (b[1] - a[1]) * p]),
        (Ch::Color(Some(a)), Ch::Color(Some(b))) => Ch::Color(Some(lerp_color(a, b, p))),
        _ => b,
    }
}

/// The props a channel mask touches (bits `1 << Prop`).
pub(crate) fn props_of(channels: u16) -> u16 {
    use value_field::*;
    let any = |bits: u16, p: Prop| {
        if channels & bits != 0 {
            1 << p as u16
        } else {
            0
        }
    };
    any(FILL, Prop::Fill)
        | any(BORDER_COLOR, Prop::BorderColor)
        | any(COLOR, Prop::Color)
        | any(OPACITY, Prop::Opacity)
        | any(TRANSLATE_X | TRANSLATE_Y, Prop::Translate)
        | any(ROTATE, Prop::Rotate)
        | any(SCALE_X | SCALE_Y, Prop::Scale)
}

/// The props keyframes can cover, in `Prop` order.
const COVERABLE: [Prop; 7] = [
    Prop::Opacity,
    Prop::Fill,
    Prop::BorderColor,
    Prop::Color,
    Prop::Translate,
    Prop::Rotate,
    Prop::Scale,
];

/// One keyframe.
#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    /// Offset in [0, 1].
    pub at: f32,
    /// The easing of the segment that starts here; `None`: the
    /// animation's.
    pub easing: Option<Easing>,
    /// The channels it sets (`CHANNELS` bits).
    pub mask: u16,
    pub values: Sample,
}

/// Ordered frames, shared by every animation that uses them (one copy
/// per transaction on the wire, `op::KEYFRAMES`).
#[derive(Clone, Debug, PartialEq)]
pub struct Keyframes {
    frames: Vec<Frame>,
    /// Every channel some frame sets.
    mask: u16,
}

impl Keyframes {
    pub fn new(frames: Vec<Frame>) -> Keyframes {
        let mask = frames.iter().fold(0, |m, f| m | f.mask);
        Keyframes { frames, mask }
    }

    pub fn frames(&self) -> &[Frame] {
        &self.frames
    }

    /// The channels some frame sets.
    pub fn mask(&self) -> u16 {
        self.mask
    }

    /// In range: 1 to `MAX_FRAMES` frames, offsets in [0, 1] and not
    /// descending, known channels, values and easings in range.
    pub fn is_valid(&self) -> bool {
        self.check().is_ok()
    }

    /// `is_valid`, with what is wrong.
    pub fn check(&self) -> Result<(), &'static str> {
        if self.frames.is_empty() {
            return Err("keyframes without a frame");
        }
        if self.frames.len() > MAX_FRAMES {
            return Err("too many keyframes");
        }
        for f in &self.frames {
            if !(0.0..=1.0).contains(&f.at) {
                return Err("keyframe offset outside [0, 1]");
            }
            if f.mask & !CHANNELS != 0 {
                return Err("keyframe channel unknown");
            }
            f.values.check(f.mask)?;
            if !f.easing.as_ref().is_none_or(Easing::is_valid) {
                return Err("keyframe easing out of range");
            }
        }
        if self.frames.windows(2).any(|w| w[0].at > w[1].at) {
            return Err("keyframe offsets out of order");
        }
        Ok(())
    }

    /// Overwrites the channels of `out` at iteration progress `p`, the
    /// implicit end frames taking `out`'s values (what lies under).
    /// `before`: CSS's before flag (`Easing::eval`).
    fn sample_into(&self, out: &mut Sample, p: f64, easing: &Easing, before: bool) {
        let mut bits = self.mask;
        while bits != 0 {
            let bit = bits & bits.wrapping_neg();
            bits &= bits - 1;
            let v = self.channel(bit, p, out.get(bit), easing, before);
            out.set(bit, v);
        }
    }

    /// Channel `bit` at progress `p`: between the last frame setting it
    /// at or before `p` and the next one after (the implicit frames at 0
    /// and 1 hold `under`; an underlying color that is unset holds the
    /// nearest frame's).
    fn channel(&self, bit: u16, p: f64, under: Ch, default: &Easing, before: bool) -> Ch {
        let mut from = (0.0, under, default);
        let mut to = None;
        for f in self.frames.iter().filter(|f| f.mask & bit != 0) {
            let v = f.values.get(bit);
            if f.at as f64 <= p {
                from = (f.at as f64, v, f.easing.as_ref().unwrap_or(default));
            } else {
                to = Some((f.at as f64, v));
                break;
            }
        }
        let (at, mut v, easing) = from;
        let (to_at, mut to_v) = to.unwrap_or((1.0, under));
        if v == Ch::Color(None) {
            v = to_v;
        }
        if to_v == Ch::Color(None) {
            to_v = v;
        }
        if to_at <= at {
            return v;
        }
        let x = (p - at) / (to_at - at);
        lerp(v, to_v, easing.eval(x, before) as f32)
    }
}

/// Playback direction.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Normal = 0,
    Reverse = 1,
    Alternate = 2,
    AlternateReverse = 3,
}

impl Direction {
    pub fn from_u8(v: u8) -> Option<Direction> {
        Some(match v {
            0 => Direction::Normal,
            1 => Direction::Reverse,
            2 => Direction::Alternate,
            3 => Direction::AlternateReverse,
            _ => return None,
        })
    }
}

/// Which phases outside the active one hold a frame's values.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fill {
    None = 0,
    /// After the end: the last sample holds.
    Forwards = 1,
    /// During the delay: the first sample holds.
    Backwards = 2,
    Both = 3,
}

impl Fill {
    pub fn from_u8(v: u8) -> Option<Fill> {
        Some(match v {
            0 => Fill::None,
            1 => Fill::Forwards,
            2 => Fill::Backwards,
            3 => Fill::Both,
            _ => return None,
        })
    }

    fn forwards(self) -> bool {
        matches!(self, Fill::Forwards | Fill::Both)
    }

    fn backwards(self) -> bool {
        matches!(self, Fill::Backwards | Fill::Both)
    }
}

/// A keyframe animation as declared: shared frames and timing. Times in
/// seconds.
#[derive(Clone, Debug, PartialEq)]
pub struct Animation {
    /// Its position in the author's list, before any entry was filtered
    /// out (a falsy one, a reduced-motion drop): with its trigger (and
    /// variant block), its identity. Ascending in a list.
    pub index: u8,
    pub keyframes: Arc<Keyframes>,
    /// Seconds before it starts; negative starts it partway through (CSS).
    pub delay: f32,
    /// One iteration. With a spring easing, the spring's settle time
    /// (`Animation::new` sets it).
    pub duration: f32,
    /// Each segment's default easing.
    pub easing: Easing,
    /// A count (fractional allowed) or `f32::INFINITY`.
    pub iterations: f32,
    pub direction: Direction,
    pub fill: Fill,
}

impl Animation {
    /// An animation over `duration` seconds, once, forwards, no fill.
    /// A spring easing replaces the duration with its settle time.
    pub fn new(keyframes: Arc<Keyframes>, duration: f32, easing: Easing) -> Animation {
        let duration = match easing {
            Easing::Spring { settle, .. } => settle,
            _ => duration,
        };
        Animation {
            index: 0,
            keyframes,
            delay: 0.0,
            duration,
            easing,
            iterations: 1.0,
            direction: Direction::Normal,
            fill: Fill::None,
        }
    }

    pub fn is_valid(&self) -> bool {
        self.check().is_ok()
    }

    /// `is_valid`, with what is wrong: a delay in [-`MAX_SECS`,
    /// `MAX_SECS`], a duration in [0, `MAX_SECS`], a finite count in [0,
    /// 1e6] or an infinite one of some duration, easing and keyframes in
    /// range.
    pub fn check(&self) -> Result<(), &'static str> {
        if !(-MAX_SECS..=MAX_SECS).contains(&self.delay) {
            return Err("animation delay out of range");
        }
        if !(0.0..=MAX_SECS).contains(&self.duration) {
            return Err("animation duration out of range");
        }
        if !self.easing.is_valid() {
            return Err("animation easing out of range");
        }
        if !(self.iterations == f32::INFINITY && self.duration > 0.0
            || (0.0..=1e6).contains(&self.iterations))
        {
            return Err("animation iterations out of range");
        }
        self.keyframes.check()
    }

    /// Runs forever.
    pub fn infinite(&self) -> bool {
        self.iterations == f32::INFINITY
    }

    /// The props it covers (bits `1 << Prop`).
    pub(crate) fn props(&self) -> u16 {
        props_of(self.keyframes.mask)
    }

    /// At `t` seconds after its start: the iteration progress it shows
    /// (`None`: none, outside the active phase without a fill that
    /// holds there), and whether its active phase is over.
    pub fn progress(&self, t: f64) -> (Option<f64>, bool) {
        let delay = self.delay as f64;
        let duration = self.duration as f64;
        let n = self.iterations as f64;
        if t < delay {
            let p = self.fill.backwards().then(|| self.directed(0.0, 0.0));
            return (p, false);
        }
        let e = t - delay;
        let active = if duration > 0.0 { duration * n } else { 0.0 };
        if e < active {
            let x = e / duration;
            let i = x.floor();
            return (Some(self.directed(i, x - i)), false);
        }
        if !self.fill.forwards() {
            return (None, true);
        }
        // The end: an integer count ends at the end of its last
        // iteration, not the start of the next.
        let i = n.floor();
        let p = if n > 0.0 && n == i {
            self.directed(i - 1.0, 1.0)
        } else {
            self.directed(i, n - i)
        };
        (Some(p), true)
    }

    /// Writes its sample at `t` seconds after its start over `out` (what
    /// lies under); false when it shows none there.
    fn sample(&self, t: f64, out: &mut Sample) -> bool {
        let Some(p) = self.progress(t).0 else {
            return false;
        };
        // CSS's before flag: in the delay, going forwards.
        let before = t < self.delay as f64
            && matches!(self.direction, Direction::Normal | Direction::Alternate);
        self.keyframes.sample_into(out, p, &self.easing, before);
        true
    }

    /// Iteration `i`'s progress at `frac` in the declared direction.
    fn directed(&self, i: f64, frac: f64) -> f64 {
        let odd = i.rem_euclid(2.0) >= 1.0;
        let reversed = match self.direction {
            Direction::Normal => false,
            Direction::Reverse => true,
            Direction::Alternate => odd,
            Direction::AlternateReverse => !odd,
        };
        if reversed { 1.0 - frac } else { frac }
    }
}

/// What started an animation: its place in the composite order.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger {
    /// Starts when the node is created (with it, in one transaction).
    Enter = 0,
    /// The node's own list (`animation`): runs while declared.
    Base = 1,
    /// A variant's: runs while the variant holds.
    Variant = 2,
}

impl Trigger {
    /// A trigger the wire names (`op::ANIMATION`): not a variant.
    pub fn from_u8(v: u8) -> Option<Trigger> {
        match v {
            0 => Some(Trigger::Enter),
            1 => Some(Trigger::Base),
            _ => None,
        }
    }
}

/// An animation's identity: its trigger, the variant block declaring it
/// (its position among the node's flattened blocks, before any was
/// skipped) and its index in the author's list (`Animation::index`).
/// The low 32 bits are its `ANIMATION_END` key without the reason:
/// index | (trigger + 1) << 16.
pub(crate) fn key(trigger: Trigger, block: u16, index: u8) -> u64 {
    index as u64 | (trigger as u64 + 1) << 16 | (block as u64) << 32
}

/// Its composite order: enter, then the node's list, then variants by
/// specificity (`position` in the table's order), each in list order.
pub(crate) fn rank(trigger: Trigger, position: usize, index: u8) -> u64 {
    (trigger as u64) << 48 | (position as u64) << 8 | index as u64
}

fn trigger_of(key: u64) -> u32 {
    (key >> 16 & 0xFF) as u32 - 1
}

/// A declared keyframe animation: running, holding its fill, or done.
#[derive(Clone, Debug)]
pub(crate) struct Running {
    key: u64,
    rank: u64,
    anim: Animation,
    start: f64,
    /// Its end is reported to JS (`out_kind::ANIMATION_END`).
    notify: bool,
    /// Past its active phase: it holds its forward fill, or stays as a
    /// tombstone (covering and showing nothing) while it is declared, so
    /// a re-send of it does not play it again.
    done: bool,
}

impl Running {
    /// It shows a sample: running, or holding its forward fill.
    fn shows(&self) -> bool {
        !self.done || self.anim.fill.forwards()
    }
}

/// A node's animations and what lies under them.
#[derive(Clone, Debug)]
pub(crate) struct NodeMotion {
    /// Ascending rank.
    list: Vec<Running>,
    /// The props some animation covers (bits `1 << Prop`).
    covered: u16,
    /// The covered props' underlying values: what the rows would hold
    /// with no keyframe animation.
    under: Sample,
    /// The underlying values changed since the last sample.
    stale: bool,
    /// The node is not drawn (`Ui::drawn`): nothing runs, covers or needs
    /// frames, and every animation starts over when it is drawn again
    /// (CSS's `display: none`).
    parked: bool,
}

impl NodeMotion {
    fn cover(&self) -> u16 {
        if self.parked {
            return 0;
        }
        self.list
            .iter()
            .filter(|r| r.shows())
            .fold(0, |m, r| m | r.anim.props())
    }

    /// Animations that need frames.
    fn live(&self) -> usize {
        if self.parked {
            return 0;
        }
        self.list.iter().filter(|r| !r.done).count()
    }

    /// The spatial pin (`Spatial::PIN_*`) it needs: an animation that
    /// has not ended holds the transform record and opacity layer of
    /// what it covers, so its frames at identity or opacity 1 (a loop's
    /// boundary) change no draw topology and allocate nothing.
    fn pin(&self) -> u8 {
        if self.parked {
            return 0;
        }
        let props = self
            .list
            .iter()
            .filter(|r| !r.done)
            .fold(0, |a, r| a | r.anim.props());
        let has = |p: Prop| props & 1 << p as u16 != 0;
        let mut pin = 0;
        if has(Prop::Translate) || has(Prop::Rotate) || has(Prop::Scale) {
            pin |= Spatial::PIN_TRANSFORM;
        }
        if has(Prop::Opacity) {
            pin |= Spatial::PIN_LAYER;
        }
        pin
    }
}

/// The keyframe driver's state.
#[derive(Default)]
pub struct Motion {
    nodes: HashMap<u32, NodeMotion>,
    /// Animations in their delay or active phase on drawn nodes: they
    /// need frames.
    live: usize,
    /// Some node's underlying values changed.
    stale: bool,
    /// Per node slot, the transaction that created its occupant.
    births: Vec<u64>,
    /// Transactions executed (`births`' clock).
    txn: u64,
    /// Ends found by a frame, reported after it.
    ended: Vec<(NodeId, u64)>,
    /// The structure revision the last drawn check saw.
    structure: Rev,
    /// A node gained a record since: check again.
    recheck: bool,
}

impl Motion {
    /// A transaction starts.
    pub(crate) fn begin(&mut self) {
        self.txn += 1;
    }

    /// `node` is created by the current transaction.
    pub(crate) fn born(&mut self, node: NodeId) {
        let i = node.index();
        if self.births.len() <= i {
            self.births.resize(i + 1, 0);
        }
        self.births[i] = self.txn;
    }

    /// Whether the current transaction created `node`.
    pub(crate) fn newborn(&self, node: NodeId) -> bool {
        self.births.get(node.index()) == Some(&self.txn)
    }

    /// Drops a node's animations, unreported (a recycled slot starts
    /// clean).
    pub(crate) fn forget(&mut self, node: NodeId) {
        if let Some(m) = self.nodes.remove(&node.0) {
            self.live -= m.live();
        }
    }

    /// Animations that need frames.
    pub fn live(&self) -> usize {
        self.live
    }

    /// Nodes with keyframe animations (running, holding a fill, or done
    /// while still declared).
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Whether a frame has work: an animation runs or an underlying
    /// value changed.
    pub(crate) fn busy(&self) -> bool {
        self.live > 0 || self.stale
    }

    /// Whether `prop` of `node` is covered.
    fn covers(&self, node: NodeId, prop: Prop) -> Option<&NodeMotion> {
        self.nodes
            .get(&node.0)
            .filter(|m| m.covered & 1 << prop as u16 != 0)
    }

    /// The underlying value of a covered `prop`.
    pub(crate) fn under(&self, node: NodeId, prop: Prop) -> Option<Sample> {
        self.covers(node, prop).map(|m| m.under)
    }
}

impl Ui {
    /// The keyframe driver's state (counts for tests and tools).
    pub fn motion(&self) -> &Motion {
        &self.motion
    }

    /// The node's keyframe-animatable values as the rows hold them.
    fn row_sample(&self, node: NodeId) -> Sample {
        let i = node.index();
        let (s, p) = (&self.host.spatial[i], &self.host.paint[i]);
        Sample {
            fill: p.fill,
            border: p.border_color,
            color: self.host.colors.get(&node.0).copied(),
            opacity: s.opacity,
            translate: s.parts.translate,
            rotate: s.parts.rotate,
            scale: s.parts.scale,
        }
    }

    /// Writes the `props` (bits `1 << Prop`) of `s` to the node's rows.
    fn write_sample(&mut self, node: NodeId, props: u16, s: &Sample) {
        let has = |p: Prop| props & 1 << p as u16 != 0;
        let patch = SpatialPatch {
            translate: has(Prop::Translate).then_some(s.translate),
            rotate: has(Prop::Rotate).then_some(s.rotate),
            scale: has(Prop::Scale).then_some(s.scale),
            matrix: None,
            opacity: has(Prop::Opacity).then_some(s.opacity.clamp(0.0, 1.0)),
        };
        if !patch.is_empty() {
            self.set_spatial(node, patch);
        }
        if has(Prop::Fill) || has(Prop::BorderColor) {
            self.set_paint(
                node,
                has(Prop::Fill).then_some(s.fill),
                None,
                has(Prop::BorderColor).then_some(s.border),
                None,
            );
        }
        if has(Prop::Color) {
            self.set_color(node, s.color);
        }
    }

    /// A writer sets `prop` of `node` to `v` (`None`: clears the
    /// inherited color). Returns whether a keyframe animation covers it:
    /// the value then goes under the animation, and the next frame
    /// samples over it.
    pub(crate) fn absorb(
        &mut self,
        node: NodeId,
        prop: Prop,
        v: Option<crate::animation::Value>,
    ) -> bool {
        use crate::animation::Value;
        let Some(m) = self.motion.nodes.get_mut(&node.0) else {
            return false;
        };
        if m.covered & 1 << prop as u16 == 0 {
            return false;
        }
        let u = &mut m.under;
        match (prop, v) {
            (Prop::Opacity, Some(Value::Opacity(o))) => u.opacity = o,
            (Prop::Fill, Some(Value::Color(c))) => u.fill = c,
            (Prop::BorderColor, Some(Value::Color(c))) => u.border = c,
            (Prop::Color, v) => {
                u.color = match v {
                    Some(Value::Color(c)) => Some(c),
                    _ => None,
                }
            }
            (Prop::Translate, Some(Value::Translate(t))) => u.translate = t,
            (Prop::Rotate, Some(Value::Rotate(r))) => u.rotate = r,
            (Prop::Scale, Some(Value::Scale(s))) => u.scale = s,
            _ => {}
        }
        m.stale = true;
        self.motion.stale = true;
        self.force_paint = true;
        true
    }

    /// Starts animation `anim` on `node` now (parked with it when the
    /// node is not drawn).
    fn start_keyframes(
        &mut self,
        node: NodeId,
        key: u64,
        rank: u64,
        anim: Animation,
        notify: bool,
    ) {
        // A new record: whether its node is drawn is checked next frame.
        self.motion.recheck |= !self.motion.nodes.contains_key(&node.0);
        let m = self
            .motion
            .nodes
            .entry(node.0)
            .or_insert_with(|| NodeMotion {
                list: Vec::new(),
                covered: 0,
                under: Sample::default(),
                stale: false,
                parked: false,
            });
        let at = m.list.partition_point(|r| r.rank < rank);
        m.list.insert(
            at,
            Running {
                key,
                rank,
                anim,
                start: self.time,
                notify,
                done: false,
            },
        );
        if !m.parked {
            self.motion.live += 1;
        }
        self.force_paint = true;
        self.recover(node);
    }

    /// Brings the node's cover up to its list: newly covered props take
    /// what the rows hold as what lies under; props nothing covers any
    /// more take theirs back at once. Drops the record when its list is
    /// empty, and sets the node's pin.
    fn recover(&mut self, node: NodeId) {
        let rows = self.row_sample(node);
        let Some(m) = self.motion.nodes.get_mut(&node.0) else {
            return;
        };
        let before = m.covered;
        m.covered = m.cover();
        let fresh = m.covered & !before;
        for p in COVERABLE {
            if fresh & 1 << p as u16 != 0 {
                m.under.copy(&rows, p);
            }
        }
        m.stale = true;
        self.motion.stale = true;
        let (gone, under, pin) = (before & !m.covered, m.under, m.pin());
        if m.list.is_empty() {
            self.motion.nodes.remove(&node.0);
        }
        if gone != 0 {
            self.write_sample(node, gone, &under);
        }
        self.pin_spatial(node, pin);
    }

    /// Ends the animation of `node` at `i` in its list (reported with
    /// `reason` when JS asked and it was running).
    fn end_keyframes(&mut self, node: NodeId, i: usize, reason: u32) {
        let Some(m) = self.motion.nodes.get_mut(&node.0) else {
            return;
        };
        let parked = m.parked;
        let r = m.list.remove(i);
        if !r.done && !parked {
            self.motion.live -= 1;
            if r.notify && !r.anim.infinite() {
                self.report_keyframes_end(node, r.key, reason);
            }
        }
        self.recover(node);
    }

    fn report_keyframes_end(&mut self, node: NodeId, key: u64, reason: u32) {
        let mut e = self.event(crate::events::out_kind::ANIMATION_END, node);
        e.key = key as u32 | reason << 8;
        self.pending_events.push(e);
    }

    /// Ends every keyframe animation of `node` (it is being removed).
    pub(crate) fn end_keyframes_of(&mut self, node: NodeId) {
        while self
            .motion
            .nodes
            .get(&node.0)
            .is_some_and(|m| !m.list.is_empty())
        {
            self.end_keyframes(node, 0, end_reason::REMOVED);
        }
    }

    /// Applies an `ANIMATION` op: `enter` starts only with the node's
    /// creation; the node's list replaces the one declared.
    pub(crate) fn declare_animations(
        &mut self,
        node: NodeId,
        trigger: Trigger,
        notify: bool,
        anims: &[Animation],
    ) {
        match trigger {
            Trigger::Enter => {
                if !self.motion.newborn(node) {
                    return;
                }
                for a in anims {
                    let (key, rank) = (key(trigger, 0, a.index), rank(trigger, 0, a.index));
                    self.start_keyframes(node, key, rank, a.clone(), notify);
                }
            }
            _ => {
                let want: Vec<(u64, u64, Animation)> = anims
                    .iter()
                    .map(|a| {
                        (
                            key(trigger, 0, a.index),
                            rank(trigger, 0, a.index),
                            a.clone(),
                        )
                    })
                    .collect();
                self.sync_keyframes(node, trigger, &want, notify);
            }
        }
    }

    /// Makes `trigger`'s animations of `node` the `want` list (key, rank,
    /// animation). One whose key is no longer wanted ends (`CANCELLED`);
    /// one wanted with other keyframes starts over (`RETARGETED`); one
    /// wanted with the same keyframes carries on, taking its new timing
    /// in place while it runs (CSS) and staying over when done; a new key
    /// starts.
    pub(crate) fn sync_keyframes(
        &mut self,
        node: NodeId,
        trigger: Trigger,
        want: &[(u64, u64, Animation)],
        notify: bool,
    ) {
        let t = trigger as u32;
        while let Some(m) = self.motion.nodes.get(&node.0) {
            let stale = m.list.iter().enumerate().find_map(|(i, r)| {
                if trigger_of(r.key) != t {
                    return None;
                }
                match want.iter().find(|w| w.0 == r.key) {
                    None => Some((i, end_reason::CANCELLED)),
                    Some(w) if w.2.keyframes != r.anim.keyframes => {
                        Some((i, end_reason::RETARGETED))
                    }
                    Some(_) => None,
                }
            });
            let Some((i, reason)) = stale else { break };
            self.end_keyframes(node, i, reason);
        }
        let mut kept = false;
        for (key, rank, anim) in want {
            let held = self
                .motion
                .nodes
                .get_mut(&node.0)
                .and_then(|m| m.list.iter_mut().find(|r| r.key == *key));
            match held {
                Some(r) => {
                    r.rank = *rank;
                    r.notify = notify;
                    if !r.done {
                        r.anim = anim.clone();
                    }
                    kept = true;
                }
                None => self.start_keyframes(node, *key, *rank, anim.clone(), notify),
            }
        }
        if let Some(m) = self.motion.nodes.get_mut(&node.0).filter(|_| kept) {
            m.list.sort_by_key(|r| r.rank);
            m.stale = true;
            self.motion.stale = true;
        }
    }

    /// Starts and stops the node's variant animations with its variants.
    pub(crate) fn sync_variant_animations(&mut self, id: u32) {
        let want = self.states.variant_animations(id);
        let declared = self.motion.nodes.get(&id).is_some_and(|m| {
            m.list
                .iter()
                .any(|r| trigger_of(r.key) == Trigger::Variant as u32)
        });
        if !want.is_empty() || declared {
            self.sync_keyframes(NodeId(id), Trigger::Variant, &want, false);
        }
    }

    /// Whether `node` is drawn: in a root's tree, with no `display: none`
    /// on it or above. Exits (next) will count an exiting subtree as
    /// drawn, though it is detached: this is the test, not attachment.
    fn drawn(&self, node: NodeId) -> bool {
        let mut cur = node;
        loop {
            let Some(n) = self.host.node(cur) else {
                return false;
            };
            if self.host.display_none(cur) {
                return false;
            }
            let p = n.parent();
            if p.is_nil() {
                return true;
            }
            if !p.is_node() {
                return false;
            }
            cur = p;
        }
    }

    /// Parks the animations of nodes no longer drawn (their running ones
    /// end, `CANCELLED`) and starts over those of nodes drawn again, as
    /// CSS does across `display: none`. Runs when the tree's structure or
    /// visibility changed, or a node gained animations.
    fn park_undrawn(&mut self) {
        let ids: Vec<u32> = self.motion.nodes.keys().copied().collect();
        for id in ids {
            let node = NodeId(id);
            let drawn = self.drawn(node);
            let Some(m) = self.motion.nodes.get_mut(&id) else {
                continue;
            };
            if m.parked != drawn {
                continue;
            }
            if drawn {
                m.parked = false;
                for r in &mut m.list {
                    r.start = self.time;
                    r.done = false;
                }
                self.motion.live += m.live();
            } else {
                self.motion.live -= m.live();
                m.parked = true;
                let ends: Vec<u64> = m
                    .list
                    .iter()
                    .filter(|r| !r.done && r.notify && !r.anim.infinite())
                    .map(|r| r.key)
                    .collect();
                for key in ends {
                    self.report_keyframes_end(node, key, end_reason::CANCELLED);
                }
            }
            self.force_paint = true;
            self.recover(node);
        }
        self.motion.recheck = false;
        self.motion.structure = self.host.revs.structure;
    }

    /// Samples every drawn node with keyframe animations and writes their
    /// rows (the top of `render`, after the tweens wrote the underlying
    /// values). Marks finished animations done; one without a forward
    /// fill lets go of its props. Nodes with nothing running and nothing
    /// changed are skipped; idle, nothing runs.
    pub(crate) fn run_keyframes(&mut self) {
        if (self.motion.recheck || self.motion.structure != self.host.revs.structure)
            && !self.motion.nodes.is_empty()
        {
            self.park_undrawn();
        }
        if !self.motion.busy() {
            return;
        }
        let now = self.time;
        let mut nodes = std::mem::take(&mut self.motion.nodes);
        let mut ended = std::mem::take(&mut self.motion.ended);
        let mut emptied = false;
        for (&id, m) in nodes.iter_mut() {
            let mut touched = std::mem::take(&mut m.stale);
            if m.parked {
                continue;
            }
            let mut finished = false;
            for r in m.list.iter_mut().filter(|r| !r.done) {
                touched = true;
                if r.anim.progress(now - r.start).1 {
                    r.done = true;
                    self.motion.live -= 1;
                    finished = true;
                    if r.notify {
                        ended.push((NodeId(id), r.key));
                    }
                }
            }
            if !touched {
                continue;
            }
            let before = m.covered;
            if finished {
                // A done `enter` is never declared again: nothing to
                // keep unless it holds its fill.
                let enter = Trigger::Enter as u32;
                m.list.retain(|r| r.shows() || trigger_of(r.key) != enter);
                emptied |= m.list.is_empty();
                m.covered = m.cover();
            }
            let mut out = m.under;
            for r in m.list.iter().filter(|r| r.shows()) {
                r.anim.sample(now - r.start, &mut out);
            }
            // Released props show what lies under; the rest the sample.
            let gone = before & !m.covered;
            if gone != 0 {
                self.write_sample(NodeId(id), gone, &m.under);
            }
            self.write_sample(NodeId(id), m.covered, &out);
            if finished {
                self.pin_spatial(NodeId(id), m.pin());
            }
        }
        if emptied {
            nodes.retain(|_, m| !m.list.is_empty());
        }
        self.motion.nodes = nodes;
        self.motion.stale = false;
        for (node, key) in ended.drain(..) {
            self.report_keyframes_end(node, key, end_reason::FINISHED);
        }
        self.motion.ended = ended;
        self.force_paint = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(at: f32, mask: u16, f: impl FnOnce(&mut Sample)) -> Frame {
        let mut values = Sample::default();
        f(&mut values);
        Frame {
            at,
            easing: None,
            mask,
            values,
        }
    }

    fn opacity(at: f32, o: f32) -> Frame {
        frame(at, value_field::OPACITY, |s| s.opacity = o)
    }

    fn anim(frames: Vec<Frame>, duration: f32) -> Animation {
        Animation::new(Arc::new(Keyframes::new(frames)), duration, Easing::LINEAR)
    }

    /// The sample at `t` over `under`.
    fn at(a: &Animation, t: f64, under: Sample) -> Option<Sample> {
        let mut out = under;
        a.sample(t, &mut out).then_some(out)
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn segments_ease_by_their_starting_frame() {
        // 0 -> 0.5 linear (the default), 0.5 -> 1 ease-in by the frame.
        let mut mid = opacity(0.5, 1.0);
        mid.easing = Some(Easing::Bezier([0.42, 0.0, 1.0, 1.0]));
        let a = anim(vec![opacity(0.0, 0.0), mid, opacity(1.0, 0.0)], 1.0);
        let under = Sample::default();
        assert!(close(at(&a, 0.25, under).unwrap().opacity, 0.5));
        // ease-in at x = 0.5 is about 0.315: 1 - 0.315 of the way down.
        let o = at(&a, 0.75, under).unwrap().opacity;
        let ease_in = Easing::Bezier([0.42, 0.0, 1.0, 1.0]).at(0.5) as f32;
        assert!(close(o, 1.0 - ease_in), "{o}");
        assert!(ease_in < 0.4);
    }

    #[test]
    fn omitted_end_frames_take_the_underlying_value() {
        // Fade up from 8 below: only a frame at 0; the end is the
        // node's resolved 0.6 and translate 0.
        let a = anim(
            vec![frame(
                0.0,
                value_field::OPACITY | value_field::TRANSLATE_Y,
                |s| {
                    s.opacity = 0.0;
                    s.translate[1] = 8.0;
                },
            )],
            0.2,
        );
        let under = Sample {
            opacity: 0.6,
            ..Sample::default()
        };
        let s = at(&a, 0.1, under).unwrap();
        assert!(close(s.opacity, 0.3) && close(s.translate[1], 4.0), "{s:?}");
        // A channel set only in the middle: from and back to what lies
        // under.
        let b = anim(vec![opacity(0.5, 0.0)], 1.0);
        assert!(close(at(&b, 0.25, under).unwrap().opacity, 0.3));
        assert!(close(at(&b, 0.75, under).unwrap().opacity, 0.3));
        // An unset inherited color holds the frame's.
        let c = anim(
            vec![frame(0.0, value_field::COLOR, |s| {
                s.color = Some(0xFF00_00FF)
            })],
            1.0,
        );
        assert_eq!(at(&c, 0.5, under).unwrap().color, Some(0xFF00_00FF));
    }

    #[test]
    fn steps_and_linear_points() {
        let end = Easing::Steps {
            n: 4,
            jump: jump::END,
        };
        let start = Easing::Steps {
            n: 4,
            jump: jump::START,
        };
        let none = Easing::Steps {
            n: 5,
            jump: jump::NONE,
        };
        let both = Easing::Steps {
            n: 3,
            jump: jump::BOTH,
        };
        assert_eq!(
            [end.at(0.0), end.at(0.3), end.at(0.99), end.at(1.0)],
            [0.0, 0.25, 0.75, 1.0]
        );
        assert_eq!(
            [start.at(0.0), start.at(0.3), start.at(0.99)],
            [0.25, 0.5, 1.0]
        );
        assert_eq!([none.at(0.0), none.at(0.5), none.at(1.0)], [0.0, 0.5, 1.0]);
        assert_eq!([both.at(0.0), both.at(0.5), both.at(1.0)], [0.25, 0.5, 1.0]);
        // linear(0, 0.25 75%, 1): steep then slow.
        let lin = Easing::Linear(Arc::from([[0.0, 0.0], [0.75, 0.25], [1.0, 1.0]]));
        assert!(lin.is_valid());
        assert!((lin.at(0.375) - 0.125).abs() < 1e-9);
        assert!((lin.at(0.875) - 0.625).abs() < 1e-9);
        assert_eq!(lin.at(1.0), 1.0);
        // A frame easing of steps holds each value.
        let mut f0 = opacity(0.0, 0.0);
        f0.easing = Some(end);
        let a = anim(vec![f0, opacity(1.0, 1.0)], 1.0);
        assert!(close(at(&a, 0.6, Sample::default()).unwrap().opacity, 0.5));
        // CSS's before flag: in a backwards-filled delay, jump-start
        // shows the first frame, not the first step; once running, the
        // first step.
        let mut a = anim(vec![opacity(0.0, 0.0), opacity(1.0, 1.0)], 1.0);
        a.easing = start;
        a.delay = 1.0;
        a.fill = Fill::Backwards;
        let o = |t: f64| at(&a, t, Sample::default()).unwrap().opacity;
        assert_eq!([o(0.5), o(1.0), o(1.3)], [0.0, 0.25, 0.5]);
    }

    #[test]
    fn iterations_and_directions() {
        // 2.5 alternating iterations of 0 -> 1 over 1 s.
        let mut a = anim(vec![opacity(0.0, 0.0), opacity(1.0, 1.0)], 1.0);
        a.iterations = 2.5;
        a.direction = Direction::Alternate;
        a.fill = Fill::Forwards;
        let o = |a: &Animation, t: f64| at(a, t, Sample::default()).map(|s| s.opacity);
        assert!(close(o(&a, 0.25).unwrap(), 0.25));
        assert!(close(o(&a, 1.25).unwrap(), 0.75), "the second runs back");
        assert!(close(o(&a, 2.25).unwrap(), 0.25));
        // It ends halfway through the third (forward) iteration, held.
        assert!(!a.progress(2.49).1);
        assert!(a.progress(2.5).1);
        assert!(close(o(&a, 9.0).unwrap(), 0.5));
        // Two whole iterations end at the end of the second: reversed, 0.
        a.iterations = 2.0;
        assert!(close(o(&a, 5.0).unwrap(), 0.0));
        a.direction = Direction::AlternateReverse;
        assert!(close(o(&a, 0.25).unwrap(), 0.75));
        assert!(close(o(&a, 5.0).unwrap(), 1.0));
        a.direction = Direction::Reverse;
        assert!(close(o(&a, 1.25).unwrap(), 0.75));
        // Infinite never ends.
        a.iterations = f32::INFINITY;
        assert!(a.is_valid());
        assert!(!a.progress(1e6).1);
        a.duration = 0.0;
        assert!(!a.is_valid(), "an infinite loop of nothing");
    }

    #[test]
    fn fills_hold_before_and_after() {
        let mut a = anim(vec![opacity(0.0, 0.2), opacity(1.0, 0.8)], 1.0);
        a.delay = 1.0;
        let o = |a: &Animation, t: f64| at(a, t, Sample::default()).map(|s| s.opacity);
        for (fill, before, after) in [
            (Fill::None, None, None),
            (Fill::Forwards, None, Some(0.8)),
            (Fill::Backwards, Some(0.2), None),
            (Fill::Both, Some(0.2), Some(0.8)),
        ] {
            a.fill = fill;
            assert_eq!(o(&a, 0.5), before, "{fill:?} before");
            assert!(close(o(&a, 1.5).unwrap(), 0.5), "{fill:?} active");
            assert_eq!(
                o(&a, 3.0).map(|v| (v * 10.0).round() / 10.0),
                after,
                "{fill:?} after"
            );
        }
    }

    #[test]
    fn springs_set_the_duration() {
        let a = Animation::new(
            Arc::new(Keyframes::new(vec![opacity(0.0, 0.0)])),
            0.1,
            Easing::spring(170.0, 26.0, 1.0),
        );
        assert!(a.is_valid());
        assert!(a.duration > 0.3, "{}", a.duration);
        let Easing::Spring { settle, .. } = a.easing else {
            unreachable!()
        };
        assert_eq!(a.duration, settle);
        assert_eq!(a.easing.at(1.0), 1.0);
    }
}
