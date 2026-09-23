// Layer composite: draws an isolated layer's texture into its parent
// target with the layer's opacity. Both are premultiplied.

@group(0) @binding(0) var<uniform> vp: Viewport;
@group(0) @binding(1) var layer_tex: texture_2d<f32>;
@group(0) @binding(2) var layer_sampler: sampler;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VsOut {
    let corner = vec2<f32>(f32(vi & 1u), f32((vi >> 1u) & 1u));
    var out: VsOut;
    out.pos = to_ndc(mix(vp.rect.xy, vp.rect.zw, corner));
    out.uv = corner * vp.uv;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return textureSample(layer_tex, layer_sampler, in.uv) * vp.opacity;
}
