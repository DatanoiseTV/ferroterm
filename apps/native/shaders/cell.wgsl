// Instanced cell shader. One instance per cell: a pixel rect, the cell's
// foreground/background (normalized RGBA) and the glyph's atlas UVs. The unit
// quad is expanded from the vertex index; the fragment composites the glyph
// (an alpha-mask tinted by the foreground) over the background in straight
// alpha, matching the WebGL renderer exactly. A UV of u0 < 0 means "no glyph".

struct Uniforms {
    screen: vec4<f32>, // xy = target size in px, zw = grid origin (inset) in px
};

@group(0) @binding(0) var atlas_tex: texture_2d<f32>;
@group(0) @binding(1) var atlas_smp: sampler;
@group(0) @binding(2) var<uniform> U: Uniforms;

struct VOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) fg: vec4<f32>,
    @location(2) bg: vec4<f32>,
    @location(3) has_glyph: f32,
};

@vertex
fn vs(
    @builtin(vertex_index) vi: u32,
    @location(0) rect: vec4<f32>,
    @location(1) uvrect: vec4<f32>,
    @location(2) fg: vec4<f32>,
    @location(3) bg: vec4<f32>,
) -> VOut {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0), vec2<f32>(0.0, 1.0),
    );
    let corner = corners[vi];
    let p = rect.xy + corner * rect.zw + U.screen.zw;

    var out: VOut;
    out.pos = vec4<f32>(
        p.x / U.screen.x * 2.0 - 1.0,
        1.0 - p.y / U.screen.y * 2.0,
        0.0,
        1.0,
    );
    out.uv = mix(uvrect.xy, uvrect.zw, corner);
    out.fg = fg;
    out.bg = bg;
    out.has_glyph = select(0.0, 1.0, uvrect.x >= 0.0);
    return out;
}

@fragment
fn fs(in: VOut) -> @location(0) vec4<f32> {
    var ga = 0.0;
    var grgb = vec3<f32>(0.0);
    if (in.has_glyph > 0.5) {
        let cov = textureSample(atlas_tex, atlas_smp, in.uv).a;
        grgb = in.fg.rgb;
        ga = cov * in.fg.a;
    }
    let ba = in.bg.a;
    let out_a = ga + ba * (1.0 - ga);
    if (out_a <= 0.0) {
        discard;
    }
    let out_rgb = (grgb * ga + in.bg.rgb * ba * (1.0 - ga)) / out_a;
    return vec4<f32>(out_rgb, out_a);
}
