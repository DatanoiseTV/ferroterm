//! wgpu renderer: one instanced draw over the whole grid, mirroring the web
//! component's WebGL renderer (per-cell instance, glyph composited over the
//! background in the fragment shader). Device/queue are owned by the caller so
//! the same pipeline can render to a window surface or an offscreen texture
//! (used by the headless render test).

use bytemuck::{Pod, Zeroable};
use ferroterm_core::attr;

use crate::atlas::Atlas;
use crate::palette::Palette;
use crate::selection::Selection;
use crate::snapshot::Grid;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct Instance {
    rect: [f32; 4], // x, y, w, h (px)
    uv: [f32; 4],   // u0, v0, u1, v1 (u0 < 0 => no glyph)
    fg: [u8; 4],    // rgba, normalized in the shader
    bg: [u8; 4],    // rgba, a = 0 => transparent (shows cleared bg)
}

pub struct Renderer {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    uniform: wgpu::Buffer,
    atlas_tex: wgpu::Texture,
    atlas_dim: u32,
    instances: wgpu::Buffer,
    instance_cap: usize,
}

impl Renderer {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        atlas: &Atlas,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("cell"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/cell.wgsl").into()),
        });

        let dim = atlas.atlas_size();
        let atlas_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("atlas"),
            size: wgpu::Extent3d {
                width: dim,
                height: dim,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let atlas_view = atlas_tex.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("atlas-sampler"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("uniform"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("cell-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("cell-bg"),
            layout: &bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&atlas_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: uniform.as_entire_binding(),
                },
            ],
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("cell-pl"),
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("cell-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs",
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Instance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &[
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x4,
                            offset: 0,
                            shader_location: 0,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x4,
                            offset: 16,
                            shader_location: 1,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Unorm8x4,
                            offset: 32,
                            shader_location: 2,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Unorm8x4,
                            offset: 36,
                            shader_location: 3,
                        },
                    ],
                }],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs",
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        let instance_cap = 4096;
        let instances = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("instances"),
            size: (instance_cap * std::mem::size_of::<Instance>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let r = Renderer {
            pipeline,
            bind_group,
            uniform,
            atlas_tex,
            atlas_dim: dim,
            instances,
            instance_cap,
        };
        r.upload_atlas(queue, atlas);
        r
    }

    /// Set the target size and the grid origin (top-left inset) in pixels.
    pub fn set_screen(&self, queue: &wgpu::Queue, w: f32, h: f32, ox: f32, oy: f32) {
        queue.write_buffer(&self.uniform, 0, bytemuck::cast_slice(&[w, h, ox, oy]));
    }

    pub fn upload_atlas(&self, queue: &wgpu::Queue, atlas: &Atlas) {
        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &self.atlas_tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            atlas.pixels(),
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(self.atlas_dim * 4),
                rows_per_image: Some(self.atlas_dim),
            },
            wgpu::Extent3d {
                width: self.atlas_dim,
                height: self.atlas_dim,
                depth_or_array_layers: 1,
            },
        );
    }

    /// Encode a full-grid render into `view`, clearing to `clear`.
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        grid: &Grid,
        pal: &Palette,
        atlas: &mut Atlas,
        cursor_on: bool,
        clear: (u8, u8, u8),
        sel: Option<&Selection>,
        sel_top: usize,
        hover_link: u32,
    ) {
        let inst = build_instances(atlas, grid, pal, cursor_on, sel, sel_top, hover_link);
        if atlas.dirty {
            self.upload_atlas(queue, atlas);
            atlas.dirty = false;
        }
        if inst.len() > self.instance_cap {
            self.instance_cap = inst.len().next_power_of_two();
            self.instances = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("instances"),
                size: (self.instance_cap * std::mem::size_of::<Instance>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        queue.write_buffer(&self.instances, 0, bytemuck::cast_slice(&inst));

        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("frame"),
        });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("cells"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: clear.0 as f64 / 255.0,
                            g: clear.1 as f64 / 255.0,
                            b: clear.2 as f64 / 255.0,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            if !inst.is_empty() {
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &self.bind_group, &[]);
                pass.set_vertex_buffer(0, self.instances.slice(..));
                pass.draw(0..6, 0..inst.len() as u32);
            }
        }
        queue.submit(Some(enc.finish()));
    }
}

/// Turn the grid into per-cell instances (plus a cursor instance on top).
pub fn build_instances(
    atlas: &mut Atlas,
    grid: &Grid,
    pal: &Palette,
    cursor_on: bool,
    sel: Option<&Selection>,
    sel_top: usize,
    hover_link: u32,
) -> Vec<Instance> {
    let cw = atlas.cell_w as f32;
    let ch = atlas.cell_h as f32;
    // Underline / strikethrough line thickness, scaled to the cell size.
    let deco_thick = (ch / 16.0).round().max(1.0);
    let baseline = atlas.baseline() as f32;
    let mut out = Vec::with_capacity(grid.cols * grid.rows + 1);

    for y in 0..grid.rows {
        for x in 0..grid.cols {
            let c = grid.cell(x, y);
            let flags = c.flags;
            if flags & attr::WIDE_SPACER != 0 {
                continue;
            }
            let inverse = flags & attr::INVERSE != 0;
            let bold = flags & attr::BOLD != 0;
            let wide = flags & attr::WIDE != 0;
            let style = (bold as u8) | (((flags & attr::ITALIC != 0) as u8) << 1);
            let has_glyph = c.cp != 0x20 && c.cp != 0 && flags & attr::INVISIBLE == 0;
            // A hovered hyperlink underlines its whole run, even cells that
            // aren't otherwise underlined.
            let link_hover = hover_link != 0 && c.link == hover_link;
            let has_deco = flags & (attr::UNDERLINE | attr::STRIKETHROUGH) != 0 || link_hover;

            let (fg_rgb, mut bg_rgb, mut bg_a) = if inverse {
                (
                    pal.resolve(c.bg, false, false),
                    pal.resolve(c.fg, true, bold),
                    255u8,
                )
            } else {
                let filled = (c.bg >> 24) != 0;
                (
                    pal.resolve(c.fg, true, bold),
                    if filled {
                        pal.resolve(c.bg, false, false)
                    } else {
                        pal.theme.bg
                    },
                    if filled { 255 } else { 0 },
                )
            };
            // Selected cells get the selection background (opaque), so even a
            // blank cell inside the selection is highlighted. The selection is
            // stored in absolute line coordinates; `sel_top` is the absolute line
            // at the top of the current viewport, so this row's absolute line is
            // `sel_top + y`.
            let selected = sel.is_some_and(|s| !s.is_empty() && s.contains(x, sel_top + y));
            if selected {
                bg_rgb = pal.theme.selection;
                bg_a = 255;
            }
            if !has_glyph && bg_a == 0 && !has_deco {
                continue;
            }
            let fg_a = if flags & attr::DIM != 0 { 153 } else { 255 };
            let w = if wide { cw * 2.0 } else { cw };
            let (ox, oy) = (x as f32 * cw, y as f32 * ch);

            // The cell rect (glyph over background). Skip it for a bare
            // decorated cell with no glyph and a transparent background, so the
            // decoration draws over the cleared bg rather than a solid box.
            if has_glyph || bg_a != 0 {
                let uv = if has_glyph {
                    let g = atlas.glyph(c.cp, wide, style);
                    [g.u0, g.v0, g.u1, g.v1]
                } else {
                    [-1.0, -1.0, -1.0, -1.0]
                };
                out.push(Instance {
                    rect: [ox, oy, w, ch],
                    uv,
                    fg: [fg_rgb.0, fg_rgb.1, fg_rgb.2, fg_a],
                    bg: [bg_rgb.0, bg_rgb.1, bg_rgb.2, bg_a],
                });
            }

            // Underline / strikethrough: a solid fg-colored line (no glyph),
            // positioned to match the web renderer (baseline+2, 55% of cell).
            if has_deco {
                let line = |ly: f32, out: &mut Vec<Instance>| {
                    out.push(Instance {
                        rect: [ox, oy + ly, w, deco_thick],
                        uv: [-1.0, -1.0, -1.0, -1.0],
                        fg: [0, 0, 0, 0],
                        bg: [fg_rgb.0, fg_rgb.1, fg_rgb.2, fg_a],
                    });
                };
                if flags & attr::UNDERLINE != 0 || link_hover {
                    line((baseline + 2.0).min(ch - deco_thick), &mut out);
                }
                if flags & attr::STRIKETHROUGH != 0 {
                    line((ch * 0.55).round(), &mut out);
                }
            }
        }
    }

    if cursor_on
        && grid.cursor_visible
        && grid.cursor_on_screen
        && grid.cursor_y < grid.rows
        && grid.cursor_x < grid.cols
    {
        let (x, y) = (grid.cursor_x, grid.cursor_y);
        let c = grid.cell(x, y);
        let wide = c.flags & attr::WIDE != 0;
        let style =
            ((c.flags & attr::BOLD != 0) as u8) | (((c.flags & attr::ITALIC != 0) as u8) << 1);
        let w = if wide { cw * 2.0 } else { cw };
        let has_glyph = c.cp != 0x20 && c.cp != 0;
        let uv = if has_glyph {
            let g = atlas.glyph(c.cp, wide, style);
            [g.u0, g.v0, g.u1, g.v1]
        } else {
            [-1.0, -1.0, -1.0, -1.0]
        };
        let cur = pal.theme.cursor;
        let txt = pal.theme.cursor_text;
        out.push(Instance {
            rect: [x as f32 * cw, y as f32 * ch, w, ch],
            uv,
            fg: [txt.0, txt.1, txt.2, 255],
            bg: [cur.0, cur.1, cur.2, 255],
        });
    }

    out
}
