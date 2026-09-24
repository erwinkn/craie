//! Pulse in GPUI: the same demo as `examples/pulse` (React on Craie),
//! written as a GPUI developer would, for a side-by-side comparison.
//!
//! - A heat wall of 2,500 / 5,000 / 10,000 tiles, one element per tile
//!   (an absolutely placed square that grows about its center: GPUI divs
//!   have no transform). Heat drifts through color transitions (520 ms
//!   ease-out) on a tenth of the tiles every 70 ms; the pointer brushes
//!   tiles; a click (or the storm, every 1.4 s) sends a ripple through
//!   every tile. A pop is a 110 ms ease-out scale to up to 1.9, then a
//!   spring back (stiffness 320, damping 13; at rest after 1.0736 s, as
//!   Craie's driver settles it). Finished tweens stop costing work.
//! - A log of 200,000 styled entries in a variable-height `list` kept at
//!   the end, streaming 1 to 4 entries every 60 ms. (No filter field:
//!   GPUI has no text input element of its own.)
//! - 24 sparklines of 64 bars at 20 Hz (canvas quads).
//! - The wall and the log are cached views: an animation frame
//!   re-renders the wall only. The HUD and the sparklines size
//!   themselves from their content, so they are not cached.
//! - The same work as the React version: the same seeded random
//!   sequences, and periodic tasks that catch up when late (up to eight
//!   runs), both counted.
//!
//! `PULSE_CANVAS=1` paints the wall as quads in one canvas instead.
//! `PULSE_LOG=1` prints, twice a second (from a timer): wall frames per
//! second (frames that re-rendered the wall: every animation frame), the
//! wall's render-to-paint time (its render, layout, prepaint, paint; the
//! root's and other panels' work, scene finalization, and Metal encoding
//! not included), process CPU (per window and in total), and the work
//! done per second.
//!
//!   cargo run --release            (PULSE_TILES=10000 PULSE_STORM=1)

use std::cell::Cell;
use std::ops::Range;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    AnyView, App, AppContext, Application, Bounds, Context, Entity, FontWeight, HighlightStyle,
    Hsla, ListAlignment, ListState, MouseButton, MouseDownEvent, MouseMoveEvent, Pixels, Rgba,
    SharedString, StyleRefinement, StyledText, Window, WindowBounds, WindowOptions, canvas, div,
    list, point, prelude::*, px, quad, rgb, size,
};

// ------------------------------------------------------------- palette

const BG: u32 = 0x0a0c11;
const PANEL: u32 = 0x11141c;
const RAISED: u32 = 0x171b26;
const EDGE: u32 = 0x222838;
const FG: u32 = 0xe9ecf4;
const DIM: u32 = 0x8a93a8;
const FAINT: u32 = 0x566076;
const CYAN: u32 = 0x5ee6ff;
const VIOLET: u32 = 0x8a7dff;
const PINK: u32 = 0xff5cae;
const LIME: u32 = 0xa6ff5c;
const AMBER: u32 = 0xffb547;

fn color(c: u32) -> Hsla {
    rgb(c).into()
}

const HEAT_STEPS: usize = 32;

/// The heat scale, as in the React version.
fn heat_scale() -> Vec<[f32; 3]> {
    let stops: [(f32, [f32; 3]); 5] = [
        (0.0, [0x13 as f32, 0x1d as f32, 0x33 as f32]),
        (0.3, [0x1b as f32, 0x5f as f32, 0xa0 as f32]),
        (0.55, [0x5e as f32, 0xe6 as f32, 0xff as f32]),
        (0.75, [0xff as f32, 0xb5 as f32, 0x47 as f32]),
        (1.0, [0xff as f32, 0x5c as f32, 0xae as f32]),
    ];
    (0..HEAT_STEPS)
        .map(|i| {
            let t = i as f32 / (HEAT_STEPS - 1) as f32;
            let mut k = 0;
            while k < stops.len() - 2 && t > stops[k + 1].0 {
                k += 1;
            }
            let (t0, a) = stops[k];
            let (t1, b) = stops[k + 1];
            let u = (t - t0) / (t1 - t0);
            [0, 1, 2].map(|c| (a[c] + (b[c] - a[c]) * u).round() / 255.0)
        })
        .collect()
}

// ---------------------------------------------------------------- math

/// CSS cubic-bezier easing (as Craie's driver).
fn bezier(x1: f32, y1: f32, x2: f32, y2: f32, x: f32) -> f32 {
    let (cx, bx, ax) = (3.0 * x1, 3.0 * (x2 - x1) - 3.0 * x1, 1.0 - 3.0 * x2 + 3.0 * x1);
    let (cy, by, ay) = (3.0 * y1, 3.0 * (y2 - y1) - 3.0 * y1, 1.0 - 3.0 * y2 + 3.0 * y1);
    let sx = |t: f32| ((ax * t + bx) * t + cx) * t;
    let mut t = x;
    for _ in 0..8 {
        let dx = (3.0 * ax * t + 2.0 * bx) * t + cx;
        if dx.abs() < 1e-6 {
            break;
        }
        t -= (sx(t) - x) / dx;
    }
    let t = t.clamp(0.0, 1.0);
    ((ay * t + by) * t + cy) * t
}

fn ease_out(x: f32) -> f32 {
    bezier(0.0, 0.0, 0.58, 1.0, x.clamp(0.0, 1.0))
}

/// A damped spring from `from` to 1, `t` seconds in (Craie's formula).
fn spring(from: f32, t: f32) -> f32 {
    let (k, c, m) = (320.0f32, 13.0f32, 1.0f32);
    let w0 = (k / m).sqrt();
    let zeta = c / (2.0 * (k * m).sqrt());
    let wd = w0 * (1.0 - zeta * zeta).sqrt();
    let offset = (-zeta * w0 * t).exp() * ((wd * t).cos() + (zeta * w0 / wd) * (wd * t).sin());
    1.0 + (from - 1.0) * offset
}

const POP_UP: f32 = 0.110;
/// Craie's settle time for this spring (offset within 0.001).
const POP_BACK: f32 = 1.0736;
const COLOR_SECS: f32 = 0.520;

/// xorshift32, as the React version's `seeded`.
struct Rng(u32);

impl Rng {
    fn unit(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        (self.0 as f64 / 4_294_967_296.0) as f32
    }
}

/// Work done, counted per report.
#[derive(Default)]
struct Counts {
    heat: Cell<u64>,
    entries: Cell<u64>,
    sparks: Cell<u64>,
    ripples: Cell<u64>,
}

/// Runs `f` every `period`, catching up (up to eight runs) when late.
fn every<T: 'static>(cx: &mut Context<T>, period: Duration, f: fn(&mut T, &mut Context<T>)) {
    let start = Instant::now();
    cx.spawn(async move |this, cx| {
        let mut done = 0u64;
        loop {
            cx.background_executor().timer(period).await;
            let due = (start.elapsed().as_nanos() / period.as_nanos()) as u64;
            let n = (due - done).min(8);
            done = due;
            let alive = this.update(cx, |this, cx| {
                for _ in 0..n {
                    f(this, cx);
                }
            });
            if alive.is_err() {
                break;
            }
        }
    })
    .detach();
}

// ---------------------------------------------------------------- wall

const FIELD_W: f32 = 956.0;
const FIELD_H: f32 = 600.0;
const COUNTS: [usize; 3] = [2_500, 5_000, 10_000];

#[derive(Clone, Copy)]
struct Grid {
    count: usize,
    cols: usize,
    rows: usize,
    pitch: f32,
    side: f32,
}

fn grid(count: usize) -> Grid {
    let cols = ((count as f32 * FIELD_W / FIELD_H).sqrt()).ceil() as usize;
    let rows = count.div_ceil(cols);
    let pitch = ((FIELD_W / cols as f32).min(FIELD_H / rows as f32) * 4.0).floor() / 4.0;
    let gap = (pitch * 0.16).round().max(1.0);
    Grid {
        count,
        cols,
        rows,
        pitch,
        side: pitch - gap,
    }
}

struct Tile {
    heat: u8,
    from: [f32; 3],
    to: [f32; 3],
    /// Color transition start (NaN: at rest).
    color_start: f32,
    /// Pop start, delay included (NaN: at rest).
    pop_start: f32,
    pop_scale: f32,
}

struct Front {
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    r: f32,
}

struct Wall {
    epoch: Instant,
    grid: Grid,
    canvas_mode: bool,
    heat_scale: Vec<[f32; 3]>,
    tiles: Vec<Tile>,
    fronts: Vec<Front>,
    tick: u32,
    cursor: usize,
    heat_on: bool,
    storm: bool,
    storm_rng: Rng,
    bounds: Rc<Cell<Bounds<Pixels>>>,
    counts: Rc<Counts>,
    /// Tweens running in the last render.
    tweens: usize,
    meter: Meter,
}

impl Wall {
    fn new(count: usize, counts: Rc<Counts>, cx: &mut Context<Self>) -> Wall {
        let mut heat_rng = Rng(1);
        let fronts = (0..4)
            .map(|k| {
                let x = heat_rng.unit();
                let y = heat_rng.unit();
                let vx = (heat_rng.unit() - 0.5) * 0.02;
                let vy = (heat_rng.unit() - 0.5) * 0.02;
                Front {
                    x,
                    y,
                    vx,
                    vy,
                    r: 0.12 + 0.08 * k as f32,
                }
            })
            .collect();
        let mut wall = Wall {
            epoch: Instant::now(),
            grid: grid(count),
            canvas_mode: std::env::var_os("PULSE_CANVAS").is_some(),
            heat_scale: heat_scale(),
            tiles: Vec::new(),
            fronts,
            tick: 0,
            cursor: 0,
            heat_on: true,
            storm: std::env::var_os("PULSE_STORM").is_some(),
            storm_rng: Rng(2),
            bounds: Rc::new(Cell::new(Bounds::default())),
            counts,
            tweens: 0,
            meter: Meter::new(),
        };
        wall.reset_tiles();
        every(cx, Duration::from_millis(70), |this, cx| {
            if this.heat_on {
                this.heat_tick();
                cx.notify();
            }
        });
        every(cx, Duration::from_millis(500), |this, cx| this.report(cx));
        every(cx, Duration::from_millis(1400), |this, cx| {
            if this.storm {
                let c = this.storm_rng.unit() * this.grid.cols as f32;
                let r = this.storm_rng.unit() * this.grid.rows as f32;
                this.ripple(c, r);
                cx.notify();
            }
        });
        wall
    }

    fn now(&self) -> f32 {
        self.epoch.elapsed().as_secs_f32()
    }

    fn set_count(&mut self, count: usize) {
        self.grid = grid(count);
        self.reset_tiles();
    }

    fn reset_tiles(&mut self) {
        self.tiles = (0..self.grid.count)
            .map(|_| Tile {
                heat: 0,
                from: [0.0; 3],
                to: [0.0; 3],
                color_start: f32::NAN,
                pop_start: f32::NAN,
                pop_scale: 1.0,
            })
            .collect();
        for i in 0..self.grid.count {
            let v = self.sample(i);
            let c = self.heat_scale[v as usize];
            let t = &mut self.tiles[i];
            t.heat = v;
            t.from = c;
            t.to = c;
        }
    }

    fn sample(&self, i: usize) -> u8 {
        let g = self.grid;
        let x = (i % g.cols) as f32 / g.cols as f32;
        let y = (i / g.cols) as f32 / g.rows as f32;
        let t = self.tick as f32;
        let mut v = 0.08f32;
        for f in &self.fronts {
            let d2 = (x - f.x).powi(2) + (y - f.y).powi(2);
            v += (-d2 / (f.r * f.r)).exp();
        }
        v += 0.08 * (x * 23.0 + t * 0.7).sin() * (y * 17.0 - t * 0.5).cos();
        (v * 0.62 * (HEAT_STEPS - 1) as f32)
            .round()
            .clamp(0.0, (HEAT_STEPS - 1) as f32) as u8
    }

    fn heat_tick(&mut self) {
        self.counts.heat.set(self.counts.heat.get() + 1);
        self.tick += 1;
        for f in &mut self.fronts {
            f.x += f.vx;
            f.y += f.vy;
            if !(0.0..=1.0).contains(&f.x) {
                f.vx = -f.vx;
            }
            if !(0.0..=1.0).contains(&f.y) {
                f.vy = -f.vy;
            }
        }
        let g = self.grid;
        let slice = g.count.div_ceil(10);
        let now = self.now();
        for k in 0..slice {
            let i = (self.cursor + k * 10) % g.count;
            let v = self.sample(i);
            if v != self.tiles[i].heat {
                let from = self.color_now(i, now);
                let to = self.heat_scale[v as usize];
                let t = &mut self.tiles[i];
                t.heat = v;
                t.from = from;
                t.to = to;
                t.color_start = now;
            }
        }
        self.cursor = (self.cursor + 1) % 10;
    }

    fn color_now(&self, i: usize, now: f32) -> [f32; 3] {
        let t = &self.tiles[i];
        if t.color_start.is_nan() {
            return t.to;
        }
        let p = ease_out((now - t.color_start) / COLOR_SECS);
        [0, 1, 2].map(|c| t.from[c] + (t.to[c] - t.from[c]) * p)
    }

    fn pop(&mut self, i: usize, scale: f32, delay: f32, now: f32) {
        let t = &mut self.tiles[i];
        // As the React version: a tile mid-pop is left alone.
        if !t.pop_start.is_nan() {
            return;
        }
        t.pop_start = now + delay;
        t.pop_scale = scale;
    }

    fn ripple(&mut self, col: f32, row: f32) {
        self.counts.ripples.set(self.counts.ripples.get() + 1);
        let g = self.grid;
        let now = self.now();
        for i in 0..g.count {
            let d = (((i % g.cols) as f32 - col).powi(2) + ((i / g.cols) as f32 - row).powi(2))
                .sqrt();
            self.pop(i, 1.0 + 0.85 * (-d / 26.0).exp(), d * 0.011, now);
        }
    }

    fn brush(&mut self, x: f32, y: f32) {
        let g = self.grid;
        let col = (x / g.pitch).floor() as i32;
        let row = (y / g.pitch).floor() as i32;
        let reach: i32 = if g.count > 5000 { 3 } else { 2 };
        let now = self.now();
        for dr in -reach..=reach {
            for dc in -reach..=reach {
                let (c, r) = (col + dc, row + dr);
                if c < 0 || r < 0 || c >= g.cols as i32 || r >= g.rows as i32 {
                    continue;
                }
                let i = r as usize * g.cols + c as usize;
                if i < g.count && dr * dr + dc * dc <= reach * reach {
                    let d = ((dr * dr + dc * dc) as f32).sqrt();
                    self.pop(i, 1.9 - 0.25 * d, 0.0, now);
                }
            }
        }
    }

    /// Advances every tile to `now`: its color and scale; finished tweens
    /// return to rest. Returns (x, y, side, radius, color) per tile and
    /// whether anything still moves or waits.
    fn frame(&mut self, now: f32) -> (Vec<(f32, f32, f32, f32, Rgba)>, bool) {
        let g = self.grid;
        let radius = if g.side > 9.0 {
            3.0
        } else if g.side > 5.0 {
            1.5
        } else {
            1.0
        };
        let mut tweens = 0;
        let mut pending = false;
        let mut out = Vec::with_capacity(g.count);
        for (i, t) in self.tiles.iter_mut().enumerate() {
            let c = if t.color_start.is_nan() {
                t.to
            } else if now - t.color_start >= COLOR_SECS {
                t.color_start = f32::NAN;
                t.from = t.to;
                t.to
            } else {
                tweens += 1;
                let p = ease_out((now - t.color_start) / COLOR_SECS);
                [0, 1, 2].map(|k| t.from[k] + (t.to[k] - t.from[k]) * p)
            };
            let s = if t.pop_start.is_nan() {
                1.0
            } else {
                let e = now - t.pop_start;
                if e < 0.0 {
                    pending = true;
                    1.0
                } else if e < POP_UP {
                    tweens += 1;
                    1.0 + (t.pop_scale - 1.0) * ease_out(e / POP_UP)
                } else if e < POP_UP + POP_BACK {
                    tweens += 1;
                    spring(t.pop_scale, e - POP_UP)
                } else {
                    t.pop_start = f32::NAN;
                    1.0
                }
            };
            let d = g.side * s;
            let o = (g.side - d) / 2.0;
            let x = (i % g.cols) as f32 * g.pitch + o;
            let y = (i / g.cols) as f32 * g.pitch + o;
            out.push((
                x,
                y,
                d,
                radius * s,
                Rgba {
                    r: c[0],
                    g: c[1],
                    b: c[2],
                    a: 1.0,
                },
            ));
        }
        self.tweens = tweens;
        (out, tweens > 0 || pending)
    }
}

impl Wall {
    /// Records the last wall frame's cost (render start to paint end).
    fn account(&mut self) {
        let m = &mut self.meter;
        if let (Some(start), Some(end)) = (m.start, m.end.get())
            && end > start
        {
            let ms = (end - start).as_secs_f64() * 1e3;
            m.frames += 1;
            m.cost_sum += ms;
            m.cost_max = m.cost_max.max(ms);
        }
        m.end.set(None);
    }

    /// Reports twice a second, from a timer: process CPU and the work
    /// done do not depend on the wall rendering.
    fn report(&mut self, cx: &mut Context<Self>) {
        let m = &mut self.meter;
        let elapsed = m.since.elapsed().as_secs_f64();
        let cpu_now = process_cpu();
        let cpu = (cpu_now - m.cpu_at) / elapsed * 100.0;
        let n = m.frames.max(1) as f64;
        // Wall frames: frames that re-rendered the wall (every animation
        // frame does; a frame that changes only the log does not).
        let fps = (m.frames as f64 / elapsed) as f32;
        let (frame_ms, worst_ms) = (m.cost_sum / n, m.cost_max);
        let (tweens, tiles) = (self.tweens, self.grid.count);
        if let Some(hud) = &m.hud {
            hud.update(cx, |h, cx| {
                *h = Hud {
                    fps,
                    frame_ms,
                    worst_ms,
                    cpu,
                    tweens,
                    tiles,
                };
                cx.notify();
            });
        }
        if m.log {
            let per = |c: &Cell<u64>| c.replace(0) as f64 / elapsed;
            // Unbuffered: a killed measurement run keeps every line.
            let epoch = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            eprintln!(
                "[gpui] {:.0} ms: {fps:.0} fps, frame {frame_ms:.2} ms (max {worst_ms:.2}) [wall], cpu {cpu:.0}% (total {:.0} ms at epoch {epoch}), {tiles} tiles{}, {tweens} tweens; per s: heat {:.1}, entries {:.1}, sparks {:.1}, ripples {:.2}",
                m.epoch.elapsed().as_secs_f64() * 1e3,
                cpu_now * 1e3,
                if self.canvas_mode { " (canvas)" } else { "" },
                per(&self.counts.heat),
                per(&self.counts.entries),
                per(&self.counts.sparks),
                per(&self.counts.ripples),
            );
        }
        let m = &mut self.meter;
        m.since = Instant::now();
        m.cpu_at = cpu_now;
        m.frames = 0;
        m.cost_sum = 0.0;
        m.cost_max = 0.0;
    }
}

impl Render for Wall {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.account();
        self.meter.start = Some(Instant::now());
        let end_cell = self.meter.end.clone();
        // Last in the wall's paint order: closes the frame.
        let mark_end = canvas(|_, _, _| {}, move |_, _, _, _| end_cell.set(Some(Instant::now())))
            .absolute()
            .size_0();
        let now = self.now();
        let (tiles, moving) = self.frame(now);
        if moving {
            window.request_animation_frame();
        }
        let g = self.grid;
        let bounds_cell = self.bounds.clone();
        let track = canvas(move |bounds, _, _| bounds_cell.set(bounds), |_, _, _, _| {})
            .absolute()
            .size_full();
        let content = if self.canvas_mode {
            canvas(
                |_, _, _| {},
                move |bounds, _, window, _| {
                    for (x, y, d, r, c) in &tiles {
                        window.paint_quad(quad(
                            Bounds::new(
                                point(bounds.origin.x + px(*x), bounds.origin.y + px(*y)),
                                size(px(*d), px(*d)),
                            ),
                            px(*r),
                            *c,
                            px(0.0),
                            gpui::transparent_black(),
                            Default::default(),
                        ));
                    }
                },
            )
            .size_full()
            .into_any_element()
        } else {
            div()
                .relative()
                .size_full()
                .children(tiles.into_iter().map(|(x, y, d, r, c)| {
                    div()
                        .absolute()
                        .left(px(x))
                        .top(px(y))
                        .size(px(d))
                        .rounded(px(r))
                        .bg(c)
                }))
                .into_any_element()
        };
        div()
            .relative()
            .w(px(g.cols as f32 * g.pitch))
            .h(px(g.rows as f32 * g.pitch))
            .child(content)
            .child(track)
            .child(mark_end)
            .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, _, cx| {
                let b = this.bounds.get();
                if b.contains(&e.position) {
                    let p = e.position - b.origin;
                    this.brush(f32::from(p.x), f32::from(p.y));
                    cx.notify();
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, e: &MouseDownEvent, _, cx| {
                    let b = this.bounds.get();
                    let p = e.position - b.origin;
                    let g = this.grid;
                    this.ripple(
                        (f32::from(p.x) / g.pitch).floor(),
                        (f32::from(p.y) / g.pitch).floor(),
                    );
                    cx.notify();
                }),
            )
    }
}

// ----------------------------------------------------------------- log

struct LogEntry {
    text: SharedString,
    runs: Vec<(Range<usize>, HighlightStyle)>,
}

const LEVELS: [(&str, u32); 4] = [
    ("DEBUG", FAINT),
    ("INFO", CYAN),
    ("WARN", AMBER),
    ("ERROR", PINK),
];
const SERVICES: [&str; 8] = [
    "edge", "auth", "billing", "search", "render", "queue", "ledger", "gateway",
];
const VERBS: [&str; 10] = [
    "accepted", "routed", "retried", "cached", "flushed", "rebalanced", "sealed", "streamed",
    "throttled", "resolved",
];
const NOUNS: [&str; 10] = [
    "request", "session", "shard", "batch", "token", "invoice", "frame", "cursor", "lease",
    "snapshot",
];

/// The same deterministic entry as the React version.
fn entry(id: u32) -> LogEntry {
    let h = (id ^ 0x9e37_79b9).wrapping_mul(0x85eb_ca6b);
    let level = match h % 100 {
        0..55 => 1,
        55..80 => 0,
        80..94 => 2,
        _ => 3,
    };
    let ms = 36_000_000u64 + id as u64 * 137;
    let time = format!(
        "{:02}:{:02}:{:02}.{:03}",
        (ms / 3_600_000) % 24,
        (ms / 60_000) % 60,
        (ms / 1000) % 60,
        ms % 1000
    );
    let extra = if h % 7 == 0 {
        format!(
            " after {} ms; upstream {} reported {} pending",
            1 + h % 900,
            SERVICES[((h >> 7) % 8) as usize],
            h % 50
        )
    } else {
        String::new()
    };
    let service = SERVICES[((h >> 3) % 8) as usize];
    let message = format!(
        "{} {} {}{}",
        NOUNS[((h >> 11) % 10) as usize],
        (h >> 5) % 99_999,
        VERBS[((h >> 17) % 10) as usize],
        extra
    );
    let (name, level_color) = LEVELS[level];
    let mut text = String::new();
    let mut runs = Vec::new();
    let mut span = |text: &mut String, s: &str, style: HighlightStyle| {
        let start = text.len();
        text.push_str(s);
        runs.push((start..text.len(), style));
    };
    let plain = |c: u32| HighlightStyle {
        color: Some(color(c)),
        ..Default::default()
    };
    span(&mut text, &format!("{time}  "), plain(FAINT));
    span(
        &mut text,
        &format!("{name:<5}"),
        HighlightStyle {
            color: Some(color(level_color)),
            font_weight: Some(FontWeight::BOLD),
            ..Default::default()
        },
    );
    span(&mut text, &format!("  {service}  "), plain(VIOLET));
    span(&mut text, &message, plain(FG));
    LogEntry {
        text: text.into(),
        runs,
    }
}

struct Log {
    entries: Vec<LogEntry>,
    state: ListState,
    streaming: bool,
    rng: Rng,
    counts: Rc<Counts>,
}

impl Log {
    fn new(counts: Rc<Counts>, cx: &mut Context<Self>) -> Log {
        let entries: Vec<LogEntry> = (0..200_000).map(entry).collect();
        let state = ListState::new(entries.len(), ListAlignment::Bottom, px(300.0));
        every(cx, Duration::from_millis(60), |this, cx| {
            if !this.streaming {
                return;
            }
            let n = 1 + (this.rng.unit() * 4.0).floor().min(3.0) as usize;
            let old = this.entries.len();
            for k in 0..n {
                this.entries.push(entry((old + k) as u32));
            }
            this.state.splice(old..old, n);
            this.counts.entries.set(this.counts.entries.get() + n as u64);
            cx.notify();
        });
        Log {
            entries,
            state,
            streaming: true,
            rng: Rng(3),
            counts,
        }
    }
}

impl Render for Log {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let this = cx.entity();
        let len = self.entries.len();
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .items_center()
                    .px(px(14.0))
                    .py(px(12.0))
                    .child(
                        div()
                            .text_size(px(14.0))
                            .font_weight(FontWeight::BOLD)
                            .text_color(color(FG))
                            .child("Event log"),
                    )
                    .child(div().flex_grow())
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(color(DIM))
                            .child(format!("{len} entries")),
                    ),
            )
            .child(
                list(self.state.clone(), move |ix, _, cx| {
                    let e = &this.read(cx).entries[ix];
                    div()
                        .px(px(14.0))
                        .py(px(3.0))
                        .text_size(px(12.5))
                        .line_height(px(18.0))
                        .child(StyledText::new(e.text.clone()).with_highlights(e.runs.iter().cloned()))
                        .into_any_element()
                })
                .flex_grow(),
            )
    }
}

// ---------------------------------------------------------- sparklines

struct Sparks {
    series: Vec<[f32; 64]>,
    on: bool,
    rng: Rng,
    counts: Rc<Counts>,
}

impl Sparks {
    fn new(counts: Rc<Counts>, cx: &mut Context<Self>) -> Sparks {
        let mut rng = Rng(4);
        let series = (0..24)
            .map(|_| {
                let mut v = [0.0; 64];
                let mut x = 0.5f32;
                for s in &mut v {
                    x = (x + (rng.unit() - 0.5) * 0.18).clamp(0.05, 1.0);
                    *s = x;
                }
                v
            })
            .collect();
        every(cx, Duration::from_millis(50), |this, cx| {
            if !this.on {
                return;
            }
            for k in 0..this.series.len() {
                let last = this.series[k][63];
                let next = (last + (this.rng.unit() - 0.5) * 0.2).clamp(0.05, 1.0);
                let s = &mut this.series[k];
                s.copy_within(1.., 0);
                s[63] = next;
            }
            this.counts.sparks.set(this.counts.sparks.get() + 1);
            cx.notify();
        });
        Sparks {
            series,
            on: true,
            rng,
            counts,
        }
    }
}

impl Render for Sparks {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let series = self.series.clone();
        div().flex().flex_col().gap(px(8.0)).children((0..3).map(|r| {
            div().flex().gap(px(8.0)).children((0..8).map(|c| {
                let i = r * 8 + c;
                let values = series[i];
                let accent = [CYAN, VIOLET, PINK, LIME, AMBER][i % 5];
                div()
                    .flex_1()
                    .bg(color(PANEL))
                    .border_1()
                    .border_color(color(EDGE))
                    .rounded(px(10.0))
                    .px(px(9.0))
                    .py(px(6.0))
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .child(
                        div()
                            .flex()
                            .text_size(px(10.5))
                            .child(div().text_color(color(DIM)).child(format!(
                                "{}·{}",
                                ["p50", "p99", "rps", "err", "cpu", "mem", "io", "gc"][i % 8],
                                r + 1
                            )))
                            .child(div().flex_grow())
                            .child(
                                div()
                                    .font_family("Menlo")
                                    .text_color(color(accent))
                                    .child(format!("{:.1}", values[63] * 100.0)),
                            ),
                    )
                    .child(
                        canvas(
                            |_, _, _| {},
                            move |bounds, _, window, _| {
                                let max = values.iter().cloned().fold(0.0f32, f32::max);
                                let w = f32::from(bounds.size.width);
                                let h = f32::from(bounds.size.height);
                                let bw = (w - 63.0) / 64.0;
                                for (k, v) in values.iter().enumerate() {
                                    let bh = v * h;
                                    let c = if *v == max { accent } else { EDGE };
                                    window.paint_quad(gpui::fill(
                                        Bounds::new(
                                            point(
                                                bounds.origin.x + px(k as f32 * (bw + 1.0)),
                                                bounds.origin.y + px(h - bh),
                                            ),
                                            size(px(bw), px(bh)),
                                        ),
                                        color(c),
                                    ));
                                }
                            },
                        )
                        .h(px(20.0))
                        .w_full(),
                    )
            }))
        }))
    }
}

// ----------------------------------------------------------------- HUD

#[derive(Default)]
struct Hud {
    fps: f32,
    frame_ms: f64,
    worst_ms: f64,
    cpu: f64,
    tweens: usize,
    tiles: usize,
}

fn stat(label: &'static str, value: String, c: u32) -> impl IntoElement {
    div()
        .bg(color(PANEL))
        .border_1()
        .border_color(color(EDGE))
        .rounded(px(9.0))
        .px(px(10.0))
        .py(px(5.0))
        .min_w(px(92.0))
        .child(div().text_size(px(10.0)).text_color(color(FAINT)).child(label))
        .child(
            div()
                .text_size(px(15.0))
                .font_family("Menlo")
                .text_color(color(c))
                .child(value),
        )
}

impl Render for Hud {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .gap(px(8.0))
            .child(stat("WALL FRAMES / S", format!("{:.0}", self.fps), LIME))
            .child(stat("WALL RENDER→PAINT", format!("{:.2} ms", self.frame_ms), CYAN))
            .child(stat("WORST FRAME", format!("{:.2} ms", self.worst_ms), AMBER))
            .child(stat("PROCESS CPU", format!("{:.0}%", self.cpu), VIOLET))
            .child(stat("TILES", format!("{}", self.tiles), FG))
            .child(stat("TWEENS", format!("{}", self.tweens), PINK))
    }
}

/// Process CPU time (user + system), seconds.
fn process_cpu() -> f64 {
    let mut u: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: getrusage fills the struct it is given.
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut u) };
    let t = |v: libc::timeval| v.tv_sec as f64 + v.tv_usec as f64 / 1e6;
    t(u.ru_utime) + t(u.ru_stime)
}

// ---------------------------------------------------------------- root

/// Frame accounting, in the wall: with cached views, an animation frame
/// re-renders the wall and nothing else (the root included), so the
/// wall's render opens a frame and a canvas painted last in it closes
/// it. Frames where only the log or the sparklines change are not
/// timed (their cost is in process CPU).
struct Meter {
    epoch: Instant,
    since: Instant,
    cpu_at: f64,
    frames: u32,
    cost_sum: f64,
    cost_max: f64,
    start: Option<Instant>,
    end: Rc<Cell<Option<Instant>>>,
    log: bool,
    hud: Option<Entity<Hud>>,
}

impl Meter {
    fn new() -> Meter {
        Meter {
            epoch: Instant::now(),
            since: Instant::now(),
            cpu_at: process_cpu(),
            frames: 0,
            cost_sum: 0.0,
            cost_max: 0.0,
            start: None,
            end: Rc::new(Cell::new(None)),
            log: std::env::var_os("PULSE_LOG").is_some(),
            hud: None,
        }
    }
}

struct Pulse {
    wall: Entity<Wall>,
    log: Entity<Log>,
    sparks: Entity<Sparks>,
    hud: Entity<Hud>,
}

impl Pulse {
    fn new(cx: &mut Context<Self>) -> Pulse {
        let count = std::env::var("PULSE_TILES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(5_000usize);
        let counts = Rc::new(Counts::default());
        let c = counts.clone();
        let wall = cx.new(|cx| Wall::new(count, c, cx));
        let c = counts.clone();
        let log = cx.new(|cx| Log::new(c, cx));
        let c = counts.clone();
        let sparks = cx.new(|cx| Sparks::new(c, cx));
        let hud = cx.new(|_| Hud::default());
        let h = hud.clone();
        wall.update(cx, |w, _| w.meter.hud = Some(h));
        Pulse {
            wall,
            log,
            sparks,
            hud,
        }
    }

}

fn toggle(label: &'static str, on: bool, accent: u32, id: &'static str) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .px(px(12.0))
        .py(px(6.0))
        .rounded(px(9.0))
        .border_1()
        .border_color(color(if on { accent } else { EDGE }))
        .bg(color(if on { RAISED } else { PANEL }))
        .text_size(px(12.5))
        .text_color(color(if on { FG } else { DIM }))
        .hover(|s| s.bg(color(0x141824)))
        .child(label)
}

fn cached(view: impl Into<AnyView>, style: StyleRefinement) -> AnyView {
    view.into().cached(style)
}

impl Render for Pulse {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (count, heat_on, storm, canvas_mode) = {
            let w = self.wall.read(cx);
            (w.grid.count, w.heat_on, w.storm, w.canvas_mode)
        };
        let streaming = self.log.read(cx).streaming;
        let wall = self.wall.clone();
        let log = self.log.clone();
        let sparks = self.sparks.clone();

        let controls = div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .px(px(14.0))
            .py(px(10.0))
            .child(
                div()
                    .text_size(px(14.0))
                    .font_weight(FontWeight::BOLD)
                    .text_color(color(FG))
                    .child("Heat wall"),
            )
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(color(DIM))
                    .child(if canvas_mode {
                        "move to brush · click to ripple · canvas mode"
                    } else {
                        "move to brush · click to ripple"
                    }),
            )
            .child(div().flex_grow())
            .children(COUNTS.iter().map(|&c| {
                let wall = wall.clone();
                div()
                    .id(("count", c))
                    .px(px(10.0))
                    .py(px(4.0))
                    .rounded(px(7.0))
                    .bg(color(if c == count { RAISED } else { PANEL }))
                    .text_size(px(12.0))
                    .text_color(color(if c == count { CYAN } else { DIM }))
                    .child(format!("{c}"))
                    .on_click(move |_, _, cx| {
                        wall.update(cx, |w, cx| {
                            w.set_count(c);
                            cx.notify();
                        })
                    })
            }))
            .child(toggle("Heat", heat_on, PINK, "heat").on_click({
                let wall = wall.clone();
                move |_, _, cx| {
                    wall.update(cx, |w, cx| {
                        w.heat_on = !w.heat_on;
                        cx.notify();
                    })
                }
            }))
            .child(toggle("Storm", storm, AMBER, "storm").on_click({
                let wall = wall.clone();
                move |_, _, cx| {
                    wall.update(cx, |w, cx| {
                        w.storm = !w.storm;
                        cx.notify();
                    })
                }
            }))
            .child(toggle("Stream", streaming, LIME, "stream").on_click(move |_, _, cx| {
                log.update(cx, |l, cx| {
                    l.streaming = !l.streaming;
                    cx.notify();
                });
                sparks.update(cx, |s, cx| {
                    s.on = !s.on;
                    cx.notify();
                });
            }))
            .child(toggle("Ripple", false, CYAN, "ripple").on_click({
                let wall = wall.clone();
                move |_, _, cx| {
                    wall.update(cx, |w, cx| {
                        let g = w.grid;
                        w.ripple(g.cols as f32 / 2.0, g.rows as f32 / 2.0);
                        cx.notify();
                    })
                }
            }));

        let full = || {
            let mut s = StyleRefinement::default();
            s.size.width = Some(gpui::relative(1.0).into());
            s.size.height = Some(gpui::relative(1.0).into());
            s
        };
        let wall_panel = div()
            .flex()
            .flex_col()
            .flex_grow()
            .min_h(px(0.0))
            .bg(color(PANEL))
            .border_1()
            .border_color(color(EDGE))
            .rounded(px(14.0))
            .overflow_hidden()
            .child(controls)
            .child(
                div()
                    .flex()
                    .flex_grow()
                    .items_center()
                    .justify_center()
                    .child(cached(self.wall.clone(), {
                        let g = grid(count);
                        let mut s = StyleRefinement::default();
                        s.size.width = Some(px(g.cols as f32 * g.pitch).into());
                        s.size.height = Some(px(g.rows as f32 * g.pitch).into());
                        s
                    })),
            );

        let log_panel = div()
            .w(px(420.0))
            .flex_shrink_0()
            .bg(color(PANEL))
            .border_1()
            .border_color(color(EDGE))
            .rounded(px(14.0))
            .overflow_hidden()
            .child(cached(self.log.clone(), full()));


        div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .gap(px(14.0))
            .p(px(16.0))
            .bg(color(BG))
            .font_family(".SystemUIFont")
            .text_color(color(FG))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .child(
                        div()
                            .child(
                                div()
                                    .text_size(px(22.0))
                                    .font_weight(FontWeight::BOLD)
                                    .child("Pulse"),
                            )
                            .child(
                                div()
                                    .text_size(px(12.0))
                                    .text_color(color(DIM))
                                    .child("GPUI 0.2.2 · cached views"),
                            ),
                    )
                    .child(div().flex_grow())
                    // Uncached: a cached view is laid out from its style
                    // alone, and the HUD sizes itself from its content.
                    .child(self.hud.clone()),
            )
            .child(
                div()
                    .flex()
                    .gap(px(16.0))
                    .flex_grow()
                    .min_h(px(0.0))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_grow()
                            .min_h(px(0.0))
                            .gap(px(14.0))
                            .child(wall_panel)
                            // Uncached, as the HUD: sized by its content.
                            .child(self.sparks.clone()),
                    )
                    .child(log_panel),
            )
    }
}

fn main() {
    Application::new().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1440.0), px(900.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| cx.new(Pulse::new),
        )
        .unwrap();
        cx.activate(true);
    });
}
