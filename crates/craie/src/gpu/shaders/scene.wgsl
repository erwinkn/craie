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
    @location(4) color: u32,
    @location(5) page_flags: vec2<u32>,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) @interpolate(flat) page: u32,
    @location(3) @interpolate(flat) flags: u32,
};

fn unpack(c: u32) -> vec4<f32> {
    return vec4<f32>(
        f32((c >> 24u) & 0xffu) / 255.0,
        f32((c >> 16u) & 0xffu) / 255.0,
        f32((c >> 8u) & 0xffu) / 255.0,
        f32(c & 0xffu) / 255.0,
    );
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
    out.uv = mix(in.uv_min, in.uv_max, corner);
    out.color = unpack(in.color);
    out.page = in.page_flags.x;
    out.flags = in.page_flags.y;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    if (in.flags & 2u) != 0u {
        // Solid rect: no atlas fetch.
        let c = in.color;
        return vec4<f32>(c.rgb * c.a, c.a);
    }
    if (in.flags & 1u) != 0u {
        // Color bitmap glyph (emoji): RGBA straight alpha -> premultiply.
        let t = textureSample(atlas_color, atlas_sampler, in.uv, i32(in.page));
        return vec4<f32>(t.rgb * t.a, t.a);
    }
    let a = textureSample(atlas_alpha, atlas_sampler, in.uv, i32(in.page)).r;
    let c = in.color;
    return vec4<f32>(c.rgb * c.a * a, c.a * a);
}
