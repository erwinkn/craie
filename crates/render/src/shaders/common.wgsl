// Shared by scene.wgsl and composite.wgsl (concatenated at build time).

struct Viewport {
    // Target size in device px.
    size: vec2<f32>,
    // Device px of the target's top-left (non-zero for layer targets).
    origin: vec2<f32>,
    // Display scale (device px per logical unit).
    scale: f32,
    // Atlas page size in px.
    page: f32,
    // Composite: layer opacity.
    opacity: f32,
    // 1: blend in linear light (an *-srgb target encodes on store); 0:
    // blend sRGB-encoded values, as browsers do (a plain target).
    linear: f32,
    // Composite: layer bounds in device px (x0, y0, x1, y1).
    rect: vec4<f32>,
    // Composite: uv extent of the used texture region.
    uv: vec2<f32>,
    _pad1: vec2<f32>,
};

fn to_ndc(dev: vec2<f32>) -> vec4<f32> {
    let t = (dev - vp.origin) / vp.size;
    return vec4<f32>(t.x * 2.0 - 1.0, 1.0 - t.y * 2.0, 0.0, 1.0);
}
