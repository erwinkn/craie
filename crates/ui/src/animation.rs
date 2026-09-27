//! Native animation driver (ARCHITECTURE.md section 12, step 4).
//!
//! A node declares transitions per property (`Mutation::Transition`):
//! when a later mutation changes that property, the driver tweens from
//! the value on screen to the new one instead of jumping. `Animate` tweens
//! one property to a target once. Each frame (`Ui::run_animations`, at
//! the top of `render`) writes the tweened value into the node's layout,
//! spatial, or paint row through the same invalidation as a mutation:
//! the row is the resolved input, the animation record the source of
//! truth, and there is one writer per row. A layout target that is not a
//! length (`auto`, a percent) is found by one probe layout; the final
//! frame restores the declared value. Idle means no work.

use std::collections::HashMap;

use craie_core::geom::Affine;
use craie_layout::LayoutRow;
use taffy::prelude::{Dimension, LengthPercentage};

use crate::geom::Size;
use crate::host::NodeId;
use crate::ui::Ui;

/// An animatable property.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Prop {
    Transform = 0,
    Opacity = 1,
    Fill = 2,
    BorderColor = 3,
    Width = 4,
    Height = 5,
    /// All four sides.
    Padding = 6,
    /// Both axes.
    Gap = 7,
    /// The inherited text color (`COLOR`). Tweens only between two set
    /// colors: setting or clearing it jumps.
    Color = 8,
}

impl Prop {
    pub const COUNT: usize = 9;
    pub const ALL: [Prop; Prop::COUNT] = [
        Prop::Transform,
        Prop::Opacity,
        Prop::Fill,
        Prop::BorderColor,
        Prop::Width,
        Prop::Height,
        Prop::Padding,
        Prop::Gap,
        Prop::Color,
    ];

    pub fn from_u8(v: u8) -> Option<Prop> {
        use Prop::*;
        Some(match v {
            0 => Transform,
            1 => Opacity,
            2 => Fill,
            3 => BorderColor,
            4 => Width,
            5 => Height,
            6 => Padding,
            7 => Gap,
            8 => Color,
            _ => return None,
        })
    }

    /// A layout input: changes relayout the node.
    pub fn is_layout(self) -> bool {
        matches!(self, Prop::Width | Prop::Height | Prop::Padding | Prop::Gap)
    }

    /// A box paint color: only nodes with a box have one.
    pub fn is_paint(self) -> bool {
        matches!(self, Prop::Fill | Prop::BorderColor)
    }
}

/// How progress runs from 0 to 1. Times in seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Timing {
    /// A CSS `cubic-bezier(x1, y1, x2, y2)` over `duration`.
    Curve {
        delay: f32,
        duration: f32,
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
    },
    /// A damped spring from rest; it ends once within 0.001 of the
    /// target for good.
    Spring {
        delay: f32,
        stiffness: f32,
        damping: f32,
        mass: f32,
    },
}

/// Longest accepted delay, duration, or spring settle time, seconds.
pub const MAX_SECS: f32 = 600.0;
/// Spring tolerance: progress within this of 1 is at rest.
const SPRING_EPS: f64 = 1e-3;

impl Timing {
    pub const LINEAR: Timing = Timing::Curve {
        delay: 0.0,
        duration: 0.0,
        x1: 0.0,
        y1: 0.0,
        x2: 1.0,
        y2: 1.0,
    };

    /// A curve over `duration` seconds.
    pub fn curve(duration: f32, [x1, y1, x2, y2]: [f32; 4]) -> Timing {
        Timing::Curve {
            delay: 0.0,
            duration,
            x1,
            y1,
            x2,
            y2,
        }
    }

    /// The same timing after `secs` of delay.
    pub fn with_delay(mut self, secs: f32) -> Timing {
        match &mut self {
            Timing::Curve { delay, .. } | Timing::Spring { delay, .. } => *delay = secs,
        }
        self
    }

    pub fn delay(self) -> f32 {
        match self {
            Timing::Curve { delay, .. } | Timing::Spring { delay, .. } => delay,
        }
    }

    /// In range: finite, delays and durations within `MAX_SECS`, curve
    /// x control points in [0, 1] (CSS), springs damped and settling
    /// within `MAX_SECS`.
    pub fn is_valid(&self) -> bool {
        let secs = |v: f32| v.is_finite() && (0.0..=MAX_SECS).contains(&v);
        match *self {
            Timing::Curve {
                delay,
                duration,
                x1,
                y1,
                x2,
                y2,
            } => {
                secs(delay)
                    && secs(duration)
                    && (0.0..=1.0).contains(&x1)
                    && (0.0..=1.0).contains(&x2)
                    && y1.is_finite()
                    && y2.is_finite()
                    && y1.abs() <= 100.0
                    && y2.abs() <= 100.0
            }
            Timing::Spring {
                delay,
                stiffness,
                damping,
                mass,
            } => {
                let pos = |v: f32| v.is_finite() && v > 0.0 && v <= 1e6;
                secs(delay)
                    && pos(stiffness)
                    && pos(damping)
                    && pos(mass)
                    && self.run_secs() <= MAX_SECS as f64
            }
        }
    }

    /// Seconds from the end of the delay to rest.
    pub fn run_secs(&self) -> f64 {
        match *self {
            Timing::Curve { duration, .. } => duration as f64,
            Timing::Spring {
                stiffness,
                damping,
                mass,
                ..
            } => Spring::new(stiffness, damping, mass).settle(),
        }
    }

    /// Seconds from the start to rest, delay included.
    pub fn total_secs(&self) -> f64 {
        self.delay() as f64 + self.run_secs()
    }

    /// Progress at `t` seconds after the start (delay included): 0 during
    /// the delay, exactly 1 from the end on; a spring may overshoot.
    pub fn progress(&self, t: f64) -> f64 {
        let t = t - self.delay() as f64;
        if t <= 0.0 {
            return 0.0;
        }
        if t >= self.run_secs() {
            return 1.0;
        }
        match *self {
            Timing::Curve {
                duration,
                x1,
                y1,
                x2,
                y2,
                ..
            } => bezier(
                t / duration as f64,
                x1 as f64,
                y1 as f64,
                x2 as f64,
                y2 as f64,
            ),
            Timing::Spring {
                stiffness,
                damping,
                mass,
                ..
            } => 1.0 - Spring::new(stiffness, damping, mass).offset(t),
        }
    }
}

/// CSS cubic-bezier: y at x, solving x(s) = x by Newton steps with a
/// bisection fallback.
fn bezier(x: f64, x1: f64, y1: f64, x2: f64, y2: f64) -> f64 {
    let coord = |s: f64, p1: f64, p2: f64| {
        let r = 1.0 - s;
        3.0 * r * r * s * p1 + 3.0 * r * s * s * p2 + s * s * s
    };
    let slope = |s: f64, p1: f64, p2: f64| {
        let r = 1.0 - s;
        3.0 * r * r * p1 + 6.0 * r * s * (p2 - p1) + 3.0 * s * s * (1.0 - p2)
    };
    let mut s = x;
    for _ in 0..8 {
        let err = coord(s, x1, x2) - x;
        if err.abs() < 1e-9 {
            return coord(s, y1, y2);
        }
        let d = slope(s, x1, x2);
        if d.abs() < 1e-9 {
            break;
        }
        s -= err / d;
    }
    let (mut lo, mut hi) = (0.0, 1.0);
    s = x;
    for _ in 0..64 {
        let v = coord(s, x1, x2);
        if (v - x).abs() < 1e-9 {
            break;
        }
        if v < x {
            lo = s;
        } else {
            hi = s;
        }
        s = (lo + hi) * 0.5;
    }
    coord(s, y1, y2)
}

/// A damped harmonic oscillator released from offset 1 at rest.
struct Spring {
    /// Undamped angular frequency and damping ratio.
    w0: f64,
    zeta: f64,
}

impl Spring {
    fn new(stiffness: f32, damping: f32, mass: f32) -> Spring {
        let (k, c, m) = (stiffness as f64, damping as f64, mass as f64);
        Spring {
            w0: (k / m).sqrt(),
            zeta: c / (2.0 * (k * m).sqrt()),
        }
    }

    /// Offset from the target at `t` (1 at t = 0).
    fn offset(&self, t: f64) -> f64 {
        let (w0, z) = (self.w0, self.zeta);
        if z < 1.0 {
            let wd = w0 * (1.0 - z * z).sqrt();
            (-z * w0 * t).exp() * ((wd * t).cos() + (z * w0 / wd) * (wd * t).sin())
        } else if z == 1.0 {
            (-w0 * t).exp() * (1.0 + w0 * t)
        } else {
            let s = (z * z - 1.0).sqrt();
            let (r1, r2) = (-w0 * (z - s), -w0 * (z + s));
            (r2 * (r1 * t).exp() - r1 * (r2 * t).exp()) / (r2 - r1)
        }
    }

    /// Time after which the offset stays within `SPRING_EPS`: from the
    /// envelope when underdamped; otherwise the offset falls
    /// monotonically, so bisection finds it.
    fn settle(&self) -> f64 {
        let (w0, z) = (self.w0, self.zeta);
        if z < 1.0 {
            let wd = w0 * (1.0 - z * z).sqrt();
            let amp = (1.0 + (z * w0 / wd).powi(2)).sqrt();
            return ((amp / SPRING_EPS).ln() / (z * w0)).max(0.0);
        }
        let mut hi = 1.0 / w0;
        while self.offset(hi) > SPRING_EPS && hi < 1e9 {
            hi *= 2.0;
        }
        let mut lo = 0.0;
        for _ in 0..64 {
            let mid = (lo + hi) * 0.5;
            if self.offset(mid) > SPRING_EPS {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        hi
    }
}

/// A declared transition: `prop` changes tween with `timing`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transition {
    pub prop: Prop,
    pub timing: Timing,
}

/// A property value as the rows hold it (the declared value).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Value {
    Transform(Affine),
    Opacity(f32),
    /// Fill or border color, 0xRRGGBBAA.
    Color(u32),
    /// Width or height.
    Size(Dimension),
    /// left, right, top, bottom.
    Padding([LengthPercentage; 4]),
    /// column (width), row (height).
    Gap([LengthPercentage; 2]),
}

/// A value the driver interpolates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Num {
    Transform(Decomposed),
    Scalar(f32),
    Color(u32),
    Lengths([f32; 4]),
}

/// A 2D affine as rotation × upper-triangular × translation, so a
/// rotation interpolates by angle (the shorter way) and a scale by
/// factor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Decomposed {
    angle: f32,
    /// [sx, shear, sy]: the upper-triangular factor [[sx, shear], [0, sy]].
    upper: [f32; 3],
    translate: [f32; 2],
}

impl Decomposed {
    fn of(m: &Affine) -> Decomposed {
        let [a, b, c, d, e, f] = m.0;
        // M = R(angle) * [[sx, u], [0, sy]] (columns (a, b), (c, d)).
        let angle = b.atan2(a);
        let (sin, cos) = angle.sin_cos();
        // hypot: no overflow for large finite scales.
        let sx = a.hypot(b);
        let u = cos * c + sin * d;
        let sy = -sin * c + cos * d;
        Decomposed {
            angle,
            upper: [sx, u, sy],
            translate: [e, f],
        }
    }

    fn affine(&self) -> Affine {
        let (sin, cos) = self.angle.sin_cos();
        let [sx, u, sy] = self.upper;
        Affine([
            cos * sx,
            sin * sx,
            cos * u - sin * sy,
            sin * u + cos * sy,
            self.translate[0],
            self.translate[1],
        ])
    }

    fn lerp(&self, to: &Decomposed, p: f32) -> Decomposed {
        let mut delta = to.angle - self.angle;
        let tau = std::f32::consts::TAU;
        delta = (delta + std::f32::consts::PI).rem_euclid(tau) - std::f32::consts::PI;
        let l = |a: f32, b: f32| a + (b - a) * p;
        Decomposed {
            angle: self.angle + delta * p,
            upper: [
                l(self.upper[0], to.upper[0]),
                l(self.upper[1], to.upper[1]),
                l(self.upper[2], to.upper[2]),
            ],
            translate: [
                l(self.translate[0], to.translate[0]),
                l(self.translate[1], to.translate[1]),
            ],
        }
    }
}

/// Colors interpolate premultiplied (as CSS), channels clamped.
fn lerp_color(a: u32, b: u32, p: f32) -> u32 {
    let ch = |c: u32, s: u32| ((c >> s) & 0xFF) as f32;
    let (aa, ba) = (ch(a, 0), ch(b, 0));
    let alpha = (aa + (ba - aa) * p).clamp(0.0, 255.0);
    let mut out = alpha.round() as u32;
    for s in [8, 16, 24] {
        let pa = ch(a, s) * aa / 255.0;
        let pb = ch(b, s) * ba / 255.0;
        let pre = pa + (pb - pa) * p;
        let v = if alpha > 0.0 {
            (pre * 255.0 / alpha).clamp(0.0, 255.0)
        } else {
            0.0
        };
        out |= (v.round() as u32) << s;
    }
    out
}

impl Num {
    pub(crate) fn lerp(&self, to: &Num, p: f32) -> Num {
        match (self, to) {
            (Num::Transform(a), Num::Transform(b)) => Num::Transform(a.lerp(b, p)),
            (Num::Scalar(a), Num::Scalar(b)) => Num::Scalar(a + (b - a) * p),
            (Num::Color(a), Num::Color(b)) => Num::Color(lerp_color(*a, *b, p)),
            (Num::Lengths(a), Num::Lengths(b)) => {
                Num::Lengths(std::array::from_fn(|i| a[i] + (b[i] - a[i]) * p))
            }
            _ => *to,
        }
    }
}

/// One running animation. There is at most one per (node, prop).
#[derive(Clone, Debug)]
pub(crate) struct Anim {
    pub node: NodeId,
    pub prop: Prop,
    pub from: Num,
    /// `None` until the probe layout resolves a non-length target.
    pub to: Option<Num>,
    /// The declared value: the target a later mutation compares with,
    /// and what the final frame writes.
    pub declared: Value,
    pub start: f64,
    pub timing: Timing,
    /// Started by `Animate`: its end is reported to JS
    /// (`out_kind::ANIMATION_END`).
    pub notify: bool,
}

/// Why an `Animate` tween ended (`out_kind::ANIMATION_END`, key bits
/// 8..16).
pub mod end_reason {
    /// It reached its target.
    pub const FINISHED: u32 = 0;
    /// A mutation set the property with no transition.
    pub const CANCELLED: u32 = 1;
    /// Another tween of the property replaced it.
    pub const RETARGETED: u32 = 2;
    /// Its node was removed.
    pub const REMOVED: u32 = 3;
}

/// The value a row would hold for `v`: the clamps of the row writer.
fn clamped(prop: Prop, v: Num) -> Num {
    match v {
        Num::Scalar(o) if prop == Prop::Opacity => Num::Scalar(o.clamp(0.0, 1.0)),
        Num::Lengths(l) => Num::Lengths(l.map(|x| x.max(0.0))),
        v => v,
    }
}

/// Active animations (the driver's state). `index` maps (node, prop)
/// to the position in `active`, so a lookup is O(1) however many tweens
/// run: every animate command and every intercepted mutation looks one
/// up. Only `push`, `remove`, and `retain_live` change `active`.
#[derive(Default)]
pub struct Animations {
    pub(crate) active: Vec<Anim>,
    index: HashMap<(u32, Prop), usize>,
}

impl Animations {
    pub(crate) fn find(&self, node: NodeId, prop: Prop) -> Option<usize> {
        self.index.get(&(node.0, prop)).copied()
    }

    pub(crate) fn push(&mut self, a: Anim) {
        debug_assert!(self.find(a.node, a.prop).is_none(), "one per (node, prop)");
        self.index.insert((a.node.0, a.prop), self.active.len());
        self.active.push(a);
    }

    /// Removes animation `i` (the last one takes its place).
    pub(crate) fn remove(&mut self, i: usize) -> Anim {
        let a = self.active.swap_remove(i);
        self.index.remove(&(a.node.0, a.prop));
        if let Some(moved) = self.active.get(i) {
            self.index.insert((moved.node.0, moved.prop), i);
        }
        a
    }

    /// Keeps the animations `keep` accepts, in order.
    pub(crate) fn retain_live(&mut self, keep: impl Fn(&Anim) -> bool) {
        let before = self.active.len();
        self.active.retain(keep);
        if self.active.len() != before {
            self.index.clear();
            for (i, a) in self.active.iter().enumerate() {
                self.index.insert((a.node.0, a.prop), i);
            }
        }
    }

    /// The index matches `active` exactly (tests).
    #[cfg(test)]
    pub(crate) fn index_is_exact(&self) -> bool {
        self.index.len() == self.active.len()
            && self
                .active
                .iter()
                .enumerate()
                .all(|(i, a)| self.index.get(&(a.node.0, a.prop)) == Some(&i))
    }

    /// Drops a node's animations (a recycled slot starts clean).
    pub(crate) fn forget(&mut self, node: NodeId) {
        self.retain_live(|a| a.node != node);
    }

    pub fn len(&self) -> usize {
        self.active.len()
    }

    pub fn is_empty(&self) -> bool {
        self.active.is_empty()
    }
}

/// A layout length as a number, when it is one.
fn dim_length(d: Dimension) -> Option<f32> {
    match d.expand() {
        taffy::style::ExpandedDimension::Length(v) => Some(v),
        _ => None,
    }
}

fn lp_length(d: LengthPercentage) -> Option<f32> {
    match d.expand() {
        taffy::style::ExpandedLengthPercentage::Length(v) => Some(v),
        _ => None,
    }
}

impl Ui {
    /// Whether animations are running: each frame advances them.
    pub fn animating(&self) -> bool {
        !self.animations.is_empty()
    }

    /// Running tweens (declared transitions and `animate` calls).
    pub fn animation_count(&self) -> usize {
        self.animations.len()
    }

    /// The clock time at which every running animation is at rest.
    pub fn animations_end(&self) -> Option<f64> {
        self.animations
            .active
            .iter()
            .map(|a| a.start + a.timing.total_secs())
            .max_by(f64::total_cmp)
    }

    /// The node's declared transition for `prop`.
    fn transition(&self, node: NodeId, prop: Prop) -> Option<Timing> {
        self.host
            .transitions
            .get(&node.0)?
            .iter()
            .find(|t| t.prop == prop)
            .map(|t| t.timing)
    }

    /// The value in the node's row.
    pub(crate) fn row_value(&self, node: NodeId, prop: Prop) -> Value {
        let i = node.index();
        let s = &self.host.layout[i];
        match prop {
            Prop::Transform => Value::Transform(self.host.spatial[i].transform),
            Prop::Opacity => Value::Opacity(self.host.spatial[i].opacity),
            Prop::Fill => Value::Color(self.host.paint[i].fill),
            Prop::BorderColor => Value::Color(self.host.paint[i].border_color),
            Prop::Width => Value::Size(s.size().width),
            Prop::Height => Value::Size(s.size().height),
            Prop::Padding => Value::Padding([
                s.padding().left,
                s.padding().right,
                s.padding().top,
                s.padding().bottom,
            ]),
            Prop::Gap => Value::Gap([s.gap().width, s.gap().height]),
            Prop::Color => Value::Color(self.host.colors.get(&node.0).copied().unwrap_or(0)),
        }
    }

    /// The number the screen shows for `prop`: the row's value, or for a
    /// size that is not a length, the laid-out border-box size. `None`
    /// when there is none to start from (never laid out, a percent
    /// padding or gap).
    fn current_num(&self, node: NodeId, prop: Prop) -> Option<Num> {
        if prop == Prop::Color && !self.host.colors.contains_key(&node.0) {
            return None;
        }
        Some(match self.row_value(node, prop) {
            Value::Transform(t) => Num::Transform(Decomposed::of(&t)),
            Value::Opacity(o) => Num::Scalar(o),
            Value::Color(c) => Num::Color(c),
            Value::Size(d) => match dim_length(d) {
                Some(v) => Num::Lengths([v, 0.0, 0.0, 0.0]),
                None => {
                    // The laid-out size of this occupant of the slot.
                    if !self.layouts.is_laid_out(node) {
                        return None;
                    }
                    let r = self.layouts.data(node).rect.size;
                    let v = if prop == Prop::Width {
                        r.width
                    } else {
                        r.height
                    };
                    Num::Lengths([v, 0.0, 0.0, 0.0])
                }
            },
            Value::Padding(p) => {
                let v = [
                    lp_length(p[0])?,
                    lp_length(p[1])?,
                    lp_length(p[2])?,
                    lp_length(p[3])?,
                ];
                Num::Lengths(v)
            }
            Value::Gap(g) => Num::Lengths([lp_length(g[0])?, lp_length(g[1])?, 0.0, 0.0]),
        })
    }

    /// A declared value as a number, `None` when layout must resolve it.
    fn declared_num(v: &Value) -> Option<Num> {
        Some(match *v {
            Value::Transform(t) => Num::Transform(Decomposed::of(&t)),
            Value::Opacity(o) => Num::Scalar(o),
            Value::Color(c) => Num::Color(c),
            Value::Size(d) => Num::Lengths([dim_length(d)?, 0.0, 0.0, 0.0]),
            Value::Padding(p) => Num::Lengths([
                lp_length(p[0])?,
                lp_length(p[1])?,
                lp_length(p[2])?,
                lp_length(p[3])?,
            ]),
            Value::Gap(g) => Num::Lengths([lp_length(g[0])?, lp_length(g[1])?, 0.0, 0.0]),
        })
    }

    /// A mutation sets `prop` of `node` to `next`. Returns whether the
    /// caller writes it to the row now. With a running animation, its
    /// declared value is what the node holds: an equal `next` changes
    /// nothing, another one retargets (with a transition) or cancels it.
    /// Without one, a declared transition starts a tween from the value
    /// on screen. A table's first resolution (`states.snapping`) writes
    /// at once: nothing was on screen to move from.
    pub(crate) fn intercept(&mut self, node: NodeId, prop: Prop, next: Value) -> bool {
        let running = self.animations.find(node, prop);
        if self.states.snapping {
            if let Some(i) = running {
                self.end_animation(i, end_reason::CANCELLED);
            }
            return true;
        }
        if let Some(i) = running
            && self.animations.active[i].declared == next
        {
            return false;
        }
        let Some(timing) = self.transition(node, prop) else {
            if let Some(i) = running {
                self.end_animation(i, end_reason::CANCELLED);
            }
            return true;
        };
        if running.is_none() && self.row_value(node, prop) == next {
            return true;
        }
        self.start_animation(node, prop, next, timing, false)
    }

    /// Starts (or retargets) a tween of `prop` to `declared`; `notify`
    /// reports its end to JS. Returns whether the caller writes the
    /// value now instead: nothing to tween from (never laid out, a
    /// percent padding or gap), a padding or gap target that is not a
    /// length (`LEDGER.md` DF-4), or no time to tween in.
    pub(crate) fn start_animation(
        &mut self,
        node: NodeId,
        prop: Prop,
        declared: Value,
        timing: Timing,
        notify: bool,
    ) -> bool {
        let running = self.animations.find(node, prop);
        let from = match running {
            // Retarget from the value on screen now: the sample, clamped
            // as the row writer clamps it; an unresolved target has not
            // moved from its start yet.
            Some(i) => {
                let a = &self.animations.active[i];
                let p = a.timing.progress(self.time - a.start) as f32;
                Some(clamped(prop, a.to.map_or(a.from, |to| a.from.lerp(&to, p))))
            }
            None => self.current_num(node, prop),
        };
        if let Some(i) = running {
            self.end_animation(i, end_reason::RETARGETED);
        }
        let to = Self::declared_num(&declared);
        let resolvable = to.is_some() || matches!(prop, Prop::Width | Prop::Height);
        let Some(from) = from.filter(|_| resolvable) else {
            return true;
        };
        if timing.total_secs() <= 0.0 {
            return true;
        }
        self.animations.push(Anim {
            node,
            prop,
            from,
            to,
            declared,
            start: self.time,
            timing,
            notify,
        });
        self.force_paint = true;
        false
    }

    /// Removes animation `i`, reporting its end when JS started it.
    pub(crate) fn end_animation(&mut self, i: usize, reason: u32) {
        let a = self.animations.remove(i);
        if a.notify {
            self.report_end(a.node, a.prop, reason);
        }
    }

    /// Queues an `ANIMATION_END` event for JS.
    pub(crate) fn report_end(&mut self, node: NodeId, prop: Prop, reason: u32) {
        let mut e = self.event(crate::events::out_kind::ANIMATION_END, node);
        e.key = prop as u32 | reason << 8;
        self.pending_events.push(e);
    }

    /// Ends every animation of `node` (it is being removed).
    pub(crate) fn end_animations_of(&mut self, node: NodeId, reason: u32) {
        // The lowest position first, as a scan of `active` finds them.
        while let Some(i) = Prop::ALL
            .iter()
            .filter_map(|p| self.animations.find(node, *p))
            .min()
        {
            self.end_animation(i, reason);
        }
    }

    /// Advances every animation to the clock and writes the rows (the
    /// top of `render`). Targets layout must resolve first run one probe
    /// layout at `size`.
    pub(crate) fn run_animations(&mut self, size: Size) {
        if self.animations.is_empty() {
            return;
        }
        let host = &self.host;
        self.animations.retain_live(|a| host.is_live(a.node));
        if self.animations.active.iter().any(|a| a.to.is_none()) {
            self.probe_targets(size);
        }
        let now = self.time;
        let mut k = 0;
        while k < self.animations.active.len() {
            let a = self.animations.active[k].clone();
            let t = now - a.start;
            if t >= a.timing.total_secs() {
                self.write_value(a.node, a.prop, a.declared);
                self.end_animation(k, end_reason::FINISHED);
                continue;
            }
            let p = a.timing.progress(t) as f32;
            let to = a.to.unwrap_or(a.from);
            let v = a.from.lerp(&to, p);
            self.write_num(a.node, a.prop, v);
            k += 1;
        }
        self.force_paint = true;
    }

    /// Resolves size targets that are not lengths: one layout compute
    /// with every running layout animation at its declared value (so
    /// targets that depend on each other resolve together), then every
    /// row back as it was. The compute has no side effects beyond the
    /// layout results: no anchoring, scroll commands, or offset clamps;
    /// the frame's own layout pass runs those.
    fn probe_targets(&mut self, size: Size) {
        let mut saved: Vec<(NodeId, LayoutRow)> = Vec::new();
        for i in 0..self.animations.active.len() {
            let a = self.animations.active[i].clone();
            if !a.prop.is_layout() {
                continue;
            }
            if !saved.iter().any(|(n, _)| *n == a.node) {
                saved.push((a.node, self.host.layout[a.node.index()]));
            }
            self.write_value(a.node, a.prop, a.declared);
        }
        self.layouts.probing = true;
        self.compute_layout(size);
        self.layouts.probing = false;
        for a in self.animations.active.iter_mut().filter(|a| a.to.is_none()) {
            let r = self.layouts.data(a.node).rect.size;
            let v = if a.prop == Prop::Width {
                r.width
            } else {
                r.height
            };
            a.to = Some(Num::Lengths([v, 0.0, 0.0, 0.0]));
        }
        for (node, style) in saved {
            self.set_layout(node, style);
        }
        // Lists laid out by the probe cached sizes without their rows:
        // the frame lays them out again.
        let lists: Vec<u32> = self.host.lists.map.keys().copied().collect();
        for id in lists {
            if self.host.is_live(NodeId(id)) {
                self.host.mark_layout(NodeId(id));
            }
        }
        // The frame lays out in full after this (anchors, scrolls,
        // clamps) even if every row came back unchanged.
        self.laid_out = None;
    }

    /// Writes an interpolated number to the row (clamped as rows are; a
    /// transform that does not come out finite keeps the last frame).
    fn write_num(&mut self, node: NodeId, prop: Prop, v: Num) {
        let value = match (prop, clamped(prop, v)) {
            (Prop::Transform, Num::Transform(d)) => {
                let m = d.affine();
                if !m.0.iter().all(|x| x.is_finite()) {
                    return;
                }
                Value::Transform(m)
            }
            (Prop::Opacity, Num::Scalar(o)) => Value::Opacity(o),
            (Prop::Fill | Prop::BorderColor | Prop::Color, Num::Color(c)) => Value::Color(c),
            (Prop::Width | Prop::Height, Num::Lengths(l)) => Value::Size(Dimension::length(l[0])),
            (Prop::Padding, Num::Lengths(l)) => Value::Padding(l.map(LengthPercentage::length)),
            (Prop::Gap, Num::Lengths(l)) => Value::Gap([
                LengthPercentage::length(l[0]),
                LengthPercentage::length(l[1]),
            ]),
            _ => return,
        };
        self.write_value(node, prop, value);
    }

    /// The one row writer for animated values: the same invalidation as
    /// the mutation that sets the property.
    pub(crate) fn write_value(&mut self, node: NodeId, prop: Prop, v: Value) {
        match v {
            Value::Transform(t) => self.set_spatial(node, Some(t), None),
            Value::Opacity(o) => self.set_spatial(node, None, Some(o)),
            Value::Color(c) => match prop {
                Prop::Fill => self.set_paint(node, Some(c), None, None, None),
                Prop::BorderColor => self.set_paint(node, None, None, Some(c), None),
                _ => self.set_color(node, Some(c)),
            },
            _ => {
                let mut style = self.host.layout[node.index()];
                set_row_field(&mut style, prop, v);
                self.set_layout(node, style);
            }
        }
    }
}

/// Sets `prop`'s field of a layout row to `v` (a layout value).
pub(crate) fn set_row_field(style: &mut LayoutRow, prop: Prop, v: Value) {
    match v {
        Value::Size(d) if prop == Prop::Width => style.set_width(d),
        Value::Size(d) => style.set_height(d),
        Value::Padding([l, r, t, b]) => style.set_padding(taffy::Rect {
            left: l,
            right: r,
            top: t,
            bottom: b,
        }),
        Value::Gap([w, h]) => style.set_gap(taffy::Size {
            width: w,
            height: h,
        }),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: &Affine, b: &Affine) -> bool {
        a.0.iter().zip(b.0).all(|(x, y)| (x - y).abs() < 1e-4)
    }

    #[test]
    fn decomposition_round_trips() {
        let shear = Affine([1.0, 0.0, 0.7, 1.0, 3.0, -2.0]);
        for m in [
            Affine::IDENTITY,
            Affine::rotate(0.8),
            Affine::rotate(-2.9),
            Affine::scale(2.0, 0.5),
            Affine::scale(-1.0, 1.0),
            Affine::scale(1.0, -3.0),
            shear,
            Affine::rotate(1.2)
                .mul(&shear)
                .mul(&Affine::scale(0.3, 4.0)),
            Affine([0.0, 0.0, 0.0, 0.0, 5.0, 6.0]),
        ] {
            let d = Decomposed::of(&m);
            assert!(close(&d.affine(), &m), "{m:?} -> {:?}", d.affine());
        }
    }

    #[test]
    fn transforms_interpolate_by_angle_and_scale() {
        let half =
            |a: Affine, b: Affine| Decomposed::of(&a).lerp(&Decomposed::of(&b), 0.5).affine();
        let q = std::f32::consts::FRAC_PI_4;
        assert!(close(
            &half(Affine::IDENTITY, Affine::rotate(2.0 * q)),
            &Affine::rotate(q)
        ));
        assert!(close(
            &half(Affine::scale(1.0, 1.0), Affine::scale(2.0, 3.0)),
            &Affine::scale(1.5, 2.0)
        ));
        assert!(close(
            &half(Affine::translate(0.0, 0.0), Affine::translate(10.0, -4.0)),
            &Affine::translate(5.0, -2.0)
        ));
        // The shorter way round: from +170 to -170 degrees passes 180.
        let d = (170.0f32).to_radians();
        assert!(close(
            &half(Affine::rotate(d), Affine::rotate(-d)),
            &Affine::rotate(std::f32::consts::PI)
        ));
    }

    #[test]
    fn curves_follow_css_cubic_bezier() {
        // ease-in-out is symmetric: 0.5 at the middle.
        let t = Timing::curve(1.0, [0.42, 0.0, 0.58, 1.0]);
        assert!((t.progress(0.5) - 0.5).abs() < 1e-6);
        // ease at x = 0.25 (a reference value from the curve).
        let ease = Timing::curve(1.0, [0.25, 0.1, 0.25, 1.0]);
        assert!(
            (ease.progress(0.25) - 0.4085).abs() < 1e-3,
            "{}",
            ease.progress(0.25)
        );
        // linear; delay holds 0; the end is exactly 1.
        let lin = Timing::curve(2.0, [0.0, 0.0, 1.0, 1.0]).with_delay(0.5);
        assert_eq!(lin.progress(0.4), 0.0);
        assert!((lin.progress(1.5) - 0.5).abs() < 1e-6);
        assert_eq!(lin.progress(2.5), 1.0);
        assert_eq!(lin.total_secs(), 2.5);
    }

    #[test]
    fn springs_settle_where_they_say() {
        for (k, c, m) in [
            (170.0, 26.0, 1.0),
            (100.0, 5.0, 1.0),
            (100.0, 20.0, 1.0),
            (100.0, 60.0, 1.0),
        ] {
            let t = Timing::Spring {
                delay: 0.0,
                stiffness: k,
                damping: c,
                mass: m,
            };
            assert!(t.is_valid());
            let end = t.run_secs();
            // At rest from the settle time on (sampled), not before it
            // for the monotone cases.
            for i in 0..200 {
                let s = end + i as f64 * 0.01;
                let spring = Spring::new(k, c, m);
                assert!(spring.offset(s).abs() <= SPRING_EPS + 1e-9, "{k} {c}: {s}");
            }
            if c >= 20.0 {
                let spring = Spring::new(k, c, m);
                assert!(spring.offset(end * 0.9) > SPRING_EPS);
            }
        }
        // Underdamped springs overshoot.
        let bouncy = Timing::Spring {
            delay: 0.0,
            stiffness: 100.0,
            damping: 5.0,
            mass: 1.0,
        };
        assert!((0..100).any(|i| bouncy.progress(i as f64 * 0.02) > 1.05));
        // An undamped spring never settles: invalid.
        assert!(
            !Timing::Spring {
                delay: 0.0,
                stiffness: 100.0,
                damping: 0.0,
                mass: 1.0
            }
            .is_valid()
        );
    }

    #[test]
    fn colors_interpolate_premultiplied() {
        // Opaque red to transparent: the color stays red while it fades.
        let c = lerp_color(0xFF00_00FF, 0x0000_FF00, 0.5);
        assert_eq!(c, 0xFF00_0080);
        assert_eq!(lerp_color(0x0000_00FF, 0xFFFF_FFFF, 0.5), 0x8080_80FF);
    }
}
