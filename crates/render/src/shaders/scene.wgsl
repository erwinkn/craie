// Scene pipeline: rects and glyphs read from the scene's primitive pools
// through storage buffers. The instance index selects the primitive:
// bit 31 = kind (0 rect, 1 glyph), bits 0..31 = index in its pool.
// The path pipeline (`vs_path`) draws mesh triangles: the vertex index
// selects an entry of the path index pool, which names a path vertex.
// Placement -> world matrix -> device px; the chunk origin snaps to the
// device-pixel grid under axis-aligned transforms, so text stays crisp
// and a moved chunk reuses every raster.

const NONE: u32 = 0xffffffffu;

struct RectI { x: f32, y: f32, w: f32, h: f32, radius: f32, border_width: f32, fill: u32, border: u32, chunk: u32, flags: u32 };
struct GlyphI { x: f32, y: f32, raster: u32, paint: u32, chunk: u32 };
struct PlacementI { ox: f32, oy: f32, transform: u32, clip: u32 };
struct WorldI { a: f32, b: f32, c: f32, d: f32, e: f32, f: f32, flags: u32, _pad: u32 };
struct ClipI { ia: f32, ib: f32, ic: f32, id: f32, ie: f32, if_: f32, x0: f32, y0: f32, x1: f32, y1: f32, radius: f32, parent: u32, flags: u32 };
struct RasterI { xy: u32, wh: u32, page: u32, quad: u32 };
struct PathV { x: f32, y: f32, paint: u32, info: u32 };

@group(0) @binding(0) var<uniform> vp: Viewport;
@group(0) @binding(1) var<storage, read> rects: array<RectI>;
@group(0) @binding(2) var<storage, read> glyphs: array<GlyphI>;
@group(0) @binding(3) var<storage, read> paints: array<u32>;
@group(0) @binding(4) var<storage, read> placements: array<PlacementI>;
@group(0) @binding(5) var<storage, read> worlds: array<WorldI>;
@group(0) @binding(6) var<storage, read> clips: array<ClipI>;
@group(0) @binding(7) var<storage, read> rasters: array<RasterI>;
@group(0) @binding(8) var<storage, read> path_vertices: array<PathV>;
@group(0) @binding(9) var<storage, read> path_indices: array<u32>;
@group(1) @binding(0) var atlas_alpha: texture_2d_array<f32>;
@group(1) @binding(1) var atlas_color: texture_2d_array<f32>;
@group(1) @binding(2) var atlas_sampler: sampler;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    // Rect: fragment position relative to the rect center, rect-space px.
    // Glyph: atlas uv. Path: chunk-local position (gradient input).
    @location(0) local: vec2<f32>,
    @location(1) @interpolate(flat) half: vec2<f32>,
    // Rect: radius px, border px.
    @location(2) @interpolate(flat) params: vec2<f32>,
    @location(3) @interpolate(flat) color: vec4<f32>,
    @location(4) @interpolate(flat) aux: vec4<f32>,
    // kind, clip, page, flags (rect: has border; glyph: color bitmap).
    // Path: kind 2, clip, gradient record (paint index), is gradient.
    @location(5) @interpolate(flat) info: vec4<u32>,
};

// Colors are authored as sRGB 0xRRGGBBAA. Compositing is linear and
// premultiplied: decode the authored channels, multiply by alpha, and let
// the *-srgb render target encode back to sRGB on store. Glyph coverage
// (R8) is already linear.
fn srgb_decode(v: f32) -> f32 {
    if (v <= 0.04045) {
        return v / 12.92;
    }
    return pow((v + 0.055) / 1.055, 2.4);
}

fn unpack(c: u32) -> vec4<f32> {
    let r = f32((c >> 24u) & 0xffu) / 255.0;
    let g = f32((c >> 16u) & 0xffu) / 255.0;
    let b = f32((c >> 8u) & 0xffu) / 255.0;
    let a = f32(c & 0xffu) / 255.0;
    return vec4<f32>(srgb_decode(r), srgb_decode(g), srgb_decode(b), a);
}

// Signed distance to a rounded rect: p relative to center, b half-size,
// r corner radius. Negative inside.
fn sd_rect(p: vec2<f32>, b: vec2<f32>, r_in: f32) -> f32 {
    let r = min(r_in, min(b.x, b.y));
    let q = abs(p) - b + vec2<f32>(r, r);
    return length(max(q, vec2<f32>(0.0, 0.0))) + min(max(q.x, q.y), 0.0) - r;
}

fn apply(w: WorldI, p: vec2<f32>) -> vec2<f32> {
    return vec2<f32>(w.a * p.x + w.c * p.y + w.e, w.b * p.x + w.d * p.y + w.f);
}

fn linear(w: WorldI, v: vec2<f32>) -> vec2<f32> {
    return vec2<f32>(w.a * v.x + w.c * v.y, w.b * v.x + w.d * v.y);
}

// Snap to the device-pixel grid: an axis-aligned world whose space
// snaps (bit 1). Spaces that move by fractions keep fractional origins.
// WGSL `round` rounds half to even; the CPU resolver matches it.
fn snaps(w: WorldI) -> bool {
    return (w.flags & 3u) == 3u;
}

// Chunk origin in device px.
fn chunk_origin(w: WorldI, p: PlacementI) -> vec2<f32> {
    let o = apply(w, vec2<f32>(p.ox, p.oy));
    if (snaps(w)) {
        return round(o);
    }
    return o;
}

@vertex
fn vs_main(@builtin(vertex_index) vi: u32, @builtin(instance_index) ii: u32) -> VsOut {
    let corner = vec2<f32>(f32(vi & 1u), f32((vi >> 1u) & 1u));
    let kind = ii >> 31u;
    let idx = ii & 0x7fffffffu;
    var out: VsOut;
    if (kind == 0u) {
        let r = rects[idx];
        let p = placements[r.chunk];
        let w = worlds[p.transform];
        let origin = chunk_origin(w, p);
        let aligned = (w.flags & 1u) != 0u;
        let s = sqrt(abs(w.a * w.d - w.b * w.c));
        var dev: vec2<f32>;
        if (aligned) {
            var p0 = origin + linear(w, vec2<f32>(r.x, r.y));
            var p1 = origin + linear(w, vec2<f32>(r.x + r.w, r.y + r.h));
            if ((r.flags & 1u) != 0u && snaps(w)) {
                // Snap edges so fills land crisp at any scale.
                p0 = round(p0);
                p1 = round(p1);
            }
            let lo = min(p0, p1);
            let hi = max(p0, p1);
            dev = mix(lo, hi, corner);
            out.half = (hi - lo) * 0.5;
            out.local = dev - (lo + hi) * 0.5;
        } else {
            dev = origin + linear(w, vec2<f32>(r.x, r.y) + corner * vec2<f32>(r.w, r.h));
            out.half = vec2<f32>(r.w, r.h) * 0.5 * s;
            out.local = (corner - vec2<f32>(0.5, 0.5)) * vec2<f32>(r.w, r.h) * s;
        }
        out.pos = to_ndc(dev);
        out.params = vec2<f32>(r.radius * s, r.border_width * s);
        out.color = unpack(paints[r.fill]);
        var has_border = 0u;
        if (r.border != NONE) {
            out.aux = unpack(paints[r.border]);
            has_border = 1u;
        }
        out.info = vec4<u32>(0u, p.clip, 0u, has_border);
    } else {
        let g = glyphs[idx];
        let p = placements[g.chunk];
        let w = worlds[p.transform];
        let ras = rasters[g.raster];
        let size = vec2<f32>(f32(ras.wh & 0xffffu), f32(ras.wh >> 16u));
        let quad = vec2<f32>(f32(ras.quad & 0xffffu), f32(ras.quad >> 16u));
        let xy = vec2<f32>(f32(ras.xy & 0xffffu), f32(ras.xy >> 16u));
        let origin = chunk_origin(w, p);
        // Bitmaps are rasterized at the display scale: at an unscaled
        // world a quad pixel is a device pixel. A bitmap made smaller to
        // fit a page draws scaled up to its quad.
        let dev = origin + linear(w, vec2<f32>(g.x, g.y) + corner * quad / vp.scale);
        out.pos = to_ndc(dev);
        out.local = (xy + corner * size) / vp.page;
        out.color = unpack(paints[g.paint]);
        out.info = vec4<u32>(1u, p.clip, ras.page & 0xffffu, ras.page >> 16u);
    }
    return out;
}

@vertex
fn vs_path(@builtin(vertex_index) vi: u32) -> VsOut {
    let v = path_vertices[path_indices[vi]];
    let chunk = v.info & 0x7fffffffu;
    let p = placements[chunk];
    let w = worlds[p.transform];
    let dev = chunk_origin(w, p) + linear(w, vec2<f32>(v.x, v.y));
    var out: VsOut;
    out.pos = to_ndc(dev);
    out.local = vec2<f32>(v.x, v.y);
    let gradient = v.info >> 31u;
    if (gradient == 0u) {
        out.color = unpack(paints[v.paint]);
    }
    out.info = vec4<u32>(2u, p.clip, v.paint, gradient);
    return out;
}

fn pf(i: u32) -> f32 {
    return bitcast<f32>(paints[i]);
}

// Raw sRGB channels (not decoded), for gradient interpolation.
fn unpack_raw(c: u32) -> vec4<f32> {
    return vec4<f32>(
        f32((c >> 24u) & 0xffu),
        f32((c >> 16u) & 0xffu),
        f32((c >> 8u) & 0xffu),
        f32(c & 0xffu),
    ) / 255.0;
}

// A gradient record (prim.rs `gradient`) at chunk-local `local`: pad
// spread; stops interpolate premultiplied in sRGB, as browsers do; the
// result decodes to linear like every authored color.
fn gradient_color(at: u32, local: vec2<f32>) -> vec4<f32> {
    let head = paints[at];
    let kind = head & 0xffffu;
    let n = head >> 16u;
    let gp = vec2<f32>(
        pf(at + 5u) * local.x + pf(at + 7u) * local.y + pf(at + 9u),
        pf(at + 6u) * local.x + pf(at + 8u) * local.y + pf(at + 10u),
    );
    let g = vec4<f32>(pf(at + 1u), pf(at + 2u), pf(at + 3u), pf(at + 4u));
    var t: f32;
    if (kind == 0u) {
        let d = g.zw - g.xy;
        t = dot(gp - g.xy, d) / max(dot(d, d), 1e-12);
    } else {
        t = length(gp - g.xy) / max(g.z, 1e-12);
    }
    t = clamp(t, 0.0, 1.0);
    let stops = at + 11u;
    var c = unpack_raw(paints[stops + 1u]);
    var prev_o = pf(stops);
    var prev = vec4<f32>(c.rgb * c.a, c.a);
    var out = prev;
    if (t > prev_o) {
        for (var i = 1u; i < n; i = i + 1u) {
            let o = pf(stops + 2u * i);
            let ci = unpack_raw(paints[stops + 2u * i + 1u]);
            let cur = vec4<f32>(ci.rgb * ci.a, ci.a);
            if (t <= o) {
                let k = select(0.0, (t - prev_o) / (o - prev_o), o > prev_o);
                out = mix(prev, cur, k);
                break;
            }
            prev_o = o;
            prev = cur;
            out = cur;
        }
    }
    var rgb = vec3<f32>(0.0);
    if (out.a > 0.0) {
        rgb = out.rgb / out.a;
    }
    return vec4<f32>(srgb_decode(rgb.r), srgb_decode(rgb.g), srgb_decode(rgb.b), out.a);
}

// Coverage of the clip chain at a device-px position: each clip tests in
// its own space, so rotated and scaled clips stay exact.
fn clip_coverage(dev: vec2<f32>, first: u32) -> f32 {
    var cov = 1.0;
    var id = first;
    // Parents precede children in the table, so the chain ends; the
    // bound only guards against corrupt data.
    for (var i = 0u; i < 65536u && id != NONE; i = i + 1u) {
        let c = clips[id];
        let lp = vec2<f32>(c.ia * dev.x + c.ic * dev.y + c.ie, c.ib * dev.x + c.id * dev.y + c.if_);
        let lo = vec2<f32>(c.x0, c.y0);
        let hi = vec2<f32>(c.x1, c.y1);
        let center = (lo + hi) * 0.5;
        let half = (hi - lo) * 0.5;
        // An open axis (overflow visible) bounds nothing.
        // Distance in local units -> device px: a local axis changes by
        // the length of its inverse-matrix row per device pixel.
        let per_px_x = max(length(vec2<f32>(c.ia, c.ic)), 1e-6);
        let per_px_y = max(length(vec2<f32>(c.ib, c.id)), 1e-6);
        var d_px: f32;
        switch (c.flags & 3u) {
            case 1u: { d_px = (abs(lp.y - center.y) - half.y) / per_px_y; }
            case 2u: { d_px = (abs(lp.x - center.x) - half.x) / per_px_x; }
            case 3u: { d_px = -1.0e9; }
            default: {
                let px = 1.0 / sqrt(max(abs(c.ia * c.id - c.ib * c.ic), 1e-12));
                d_px = sd_rect(lp - center, half, c.radius) * px;
            }
        }
        cov = cov * clamp(0.5 - d_px, 0.0, 1.0);
        id = c.parent;
    }
    return cov;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    var cov = 1.0;
    if (in.info.y != NONE) {
        cov = clip_coverage(in.pos.xy + vp.origin, in.info.y);
        if (cov <= 0.0) {
            discard;
        }
    }
    var out: vec4<f32>;
    if (in.info.x == 2u) {
        // Path: coverage comes from multisampling.
        var c = in.color;
        if (in.info.w != 0u) {
            c = gradient_color(in.info.z, in.local);
        }
        out = vec4<f32>(c.rgb * c.a, c.a);
    } else if (in.info.x == 0u) {
        let d = sd_rect(in.local, in.half, in.params.x);
        if (in.info.w != 0u) {
            // Border ring occupies the outer params.y px of the rect.
            let total = clamp(0.5 - d, 0.0, 1.0);
            let inner = clamp(0.5 - (d + in.params.y), 0.0, 1.0);
            let fill = in.color;
            let border = in.aux;
            out = vec4<f32>(fill.rgb * fill.a, fill.a) * inner
                + vec4<f32>(border.rgb * border.a, border.a) * (total - inner);
        } else {
            let c = in.color;
            out = vec4<f32>(c.rgb * c.a, c.a) * clamp(0.5 - d, 0.0, 1.0);
        }
    } else if (in.info.w != 0u) {
        // Color bitmap glyph (emoji): the atlas stores sRGB-encoded RGBA;
        // decode then premultiply.
        let t = textureSample(atlas_color, atlas_sampler, in.local, i32(in.info.z));
        let rgb = vec3<f32>(srgb_decode(t.r), srgb_decode(t.g), srgb_decode(t.b));
        out = vec4<f32>(rgb * t.a, t.a);
    } else {
        // Alpha glyph: coverage is linear; tint with the decoded paint.
        let a = textureSample(atlas_alpha, atlas_sampler, in.local, i32(in.info.z)).r;
        let c = in.color;
        out = vec4<f32>(c.rgb * (c.a * a), c.a * a);
    }
    return out * cov;
}
