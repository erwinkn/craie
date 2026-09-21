struct Viewport {
    size: vec2<f32>,
    _pad: vec2<f32>,
};

@group(0) @binding(0) var<uniform> vp: Viewport;
@group(1) @binding(0) var atlas_alpha: texture_2d_array<f32>;
@group(1) @binding(1) var atlas_color: texture_2d_array<f32>;
@group(1) @binding(2) var atlas_sampler: sampler;

struct VsIn {
    @location(0) position: vec2<f32>,
    @location(1) size: vec2<f32>,
    @location(2) uv_min: vec2<f32>,
    @location(3) uv_max: vec2<f32>,
    @location(4) clip_min: vec2<f32>,
    @location(5) clip_max: vec2<f32>,
    @location(6) color: u32,
    @location(7) aux_color: u32,
    @location(8) params: vec4<f32>,
    @location(9) page_flags: vec2<u32>,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) px: vec2<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) aux_color: vec4<f32>,
    @location(4) position: vec2<f32>,
    @location(5) size: vec2<f32>,
    @location(6) clip_min: vec2<f32>,
    @location(7) clip_max: vec2<f32>,
    @location(8) params: vec4<f32>,
    @location(9) @interpolate(flat) page: u32,
    @location(10) @interpolate(flat) flags: u32,
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

@vertex
fn vs_main(in: VsIn, @builtin(vertex_index) vi: u32) -> VsOut {
    let corner = vec2<f32>(f32(vi & 1u), f32((vi >> 1u) & 1u));
    let px = in.position + corner * in.size;
    var out: VsOut;
    out.clip = vec4<f32>(
        px.x / vp.size.x * 2.0 - 1.0,
        1.0 - px.y / vp.size.y * 2.0,
        0.0,
        1.0,
    );
    out.px = px;
    out.uv = mix(in.uv_min, in.uv_max, corner);
    out.color = unpack(in.color);
    out.aux_color = unpack(in.aux_color);
    out.position = in.position;
    out.size = in.size;
    out.clip_min = in.clip_min;
    out.clip_max = in.clip_max;
    out.params = in.params;
    out.page = in.page_flags.x;
    out.flags = in.page_flags.y;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Clip: every instance carries its ancestor clip rect. The clip edge
    // gets 1px AA; a fully clipped fragment contributes nothing.
    let clip_r = select(0.0, in.params.z, (in.flags & 4u) != 0u);
    let cd = sd_rect(in.px - (in.clip_min + in.clip_max) * 0.5,
                     (in.clip_max - in.clip_min) * 0.5, clip_r);
    let clip_a = clamp(0.5 - cd, 0.0, 1.0);
    if (clip_a <= 0.0) {
        discard;
    }

    var out: vec4<f32>;
    if (in.flags & 2u) != 0u {
        // Solid rect: SDF coverage for rounded corners, optional border.
        let d = sd_rect(in.px - (in.position + in.size * 0.5), in.size * 0.5,
                        in.params.x);
        if ((in.flags & 8u) != 0u) {
            // Border ring occupies the outer params.y px of the rect.
            let total_cov = clamp(0.5 - d, 0.0, 1.0);
            let inner_cov = clamp(0.5 - (d + in.params.y), 0.0, 1.0);
            let ring = total_cov - inner_cov;
            let fill = in.color;
            let border = in.aux_color;
            out = vec4<f32>(fill.rgb * fill.a, fill.a) * inner_cov
                + vec4<f32>(border.rgb * border.a, border.a) * ring;
        } else {
            let c = in.color;
            out = vec4<f32>(c.rgb * c.a, c.a) * clamp(0.5 - d, 0.0, 1.0);
        }
    } else if ((in.flags & 1u) != 0u) {
        // Color bitmap glyph (emoji): the atlas stores sRGB-encoded RGBA;
        // decode then premultiply.
        let t = textureSample(atlas_color, atlas_sampler, in.uv, i32(in.page));
        let rgb = vec3<f32>(srgb_decode(t.r), srgb_decode(t.g), srgb_decode(t.b));
        out = vec4<f32>(rgb * t.a, t.a);
    } else {
        // Alpha glyph: coverage is linear; tint with the decoded run color.
        let a = textureSample(atlas_alpha, atlas_sampler, in.uv, i32(in.page)).r;
        let c = in.color;
        out = vec4<f32>(c.rgb * (c.a * a), c.a * a);
    }
    return vec4<f32>(out.rgb * clip_a, out.a * clip_a);
}
