// Inline-image pass: draw one textured quad per placed image over the already-
// rendered cells. The unit quad is expanded from the vertex index and mapped to
// a pixel rect (top-left origin, like the cell shader); the fragment samples the
// image's RGBA texture. One uniform slot per image is selected with a dynamic
// buffer offset so every image draws in a single render pass.

struct Uniforms {
    screen: vec4<f32>, // xy = target size in px, zw = grid origin (inset) in px
    rect: vec4<f32>,   // xy = image top-left in px, zw = image size in px
};

@group(0) @binding(0) var img_tex: texture_2d<f32>;
@group(0) @binding(1) var img_smp: sampler;
@group(0) @binding(2) var<uniform> U: Uniforms;

struct VOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs(@builtin(vertex_index) vi: u32) -> VOut {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0), vec2<f32>(0.0, 1.0),
    );
    let corner = corners[vi];
    let p = U.rect.xy + corner * U.rect.zw + U.screen.zw;

    var out: VOut;
    out.pos = vec4<f32>(
        p.x / U.screen.x * 2.0 - 1.0,
        1.0 - p.y / U.screen.y * 2.0,
        0.0,
        1.0,
    );
    out.uv = corner;
    return out;
}

@fragment
fn fs(in: VOut) -> @location(0) vec4<f32> {
    return textureSample(img_tex, img_smp, in.uv);
}
