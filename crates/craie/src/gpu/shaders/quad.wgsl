struct Viewport {
    size: vec2<f32>,
    _pad: vec2<f32>,
};

@group(0) @binding(0) var<uniform> vp: Viewport;

struct VsIn {
    @location(0) position: vec2<f32>,
    @location(1) size: vec2<f32>,
    @location(2) color: u32,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
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
    out.color = unpack(in.color);
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let c = in.color;
    // Premultiplied alpha output.
    return vec4<f32>(c.rgb * c.a, c.a);
}
