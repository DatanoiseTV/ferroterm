//! Inline-image layer for the native renderer. Draws placed RGBA images (Sixel
//! and Kitty raw formats) as textured quads over the already-rendered cells.
//!
//! Encoded images (iTerm2, Kitty PNG) are not drawn here: the core hands those
//! to the web front-end to decode, and the native app links no image codec yet
//! — those placements simply carry no RGBA, so this layer skips them.

use std::collections::HashMap;

/// One image to draw this frame. `rgba` holds `src_w * src_h` source pixels; the
/// texture is that size and is scaled to the `w`x`h` on-screen rect at `(x, y)`
/// (top-left, scroll-adjusted, excluding the grid inset). For raw Sixel/Kitty
/// images the source and draw sizes match; for a decoded PNG scaled into a cell
/// box they differ.
pub struct ImageQuad<'a> {
    pub id: u32,
    pub src_w: u32,
    pub src_h: u32,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub rgba: &'a [u8],
}

/// Uniform alignment for a dynamic offset (wgpu requires 256 by default).
const SLOT: u64 = 256;

/// Decode a PNG file to `(width, height, rgba)`. Used for encoded inline images
/// (iTerm2, Kitty `f=100`) since the native app links no browser to decode them.
/// Returns `None` for anything the `png` crate can't read (other formats —
/// JPEG/GIF/WebP — are a follow-up). Bounds the output to guard a hostile size.
pub fn decode_png(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let mut dec = png::Decoder::new(bytes);
    // Expand palettes / low-bit grayscale to full channels and 16-bit to 8-bit,
    // so the decoded frame is one of the 8-bit color types handled below.
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = dec.read_info().ok()?;
    let info = reader.info();
    let (w, h) = (info.width, info.height);
    if w == 0 || h == 0 || (w as u64) * (h as u64) > 64 * 1024 * 1024 {
        return None;
    }
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let frame = reader.next_frame(&mut buf).ok()?;
    buf.truncate(frame.buffer_size());
    let rgba = match frame.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => buf
            .chunks_exact(3)
            .flat_map(|p| [p[0], p[1], p[2], 0xff])
            .collect(),
        png::ColorType::Grayscale => buf.iter().flat_map(|&v| [v, v, v, 0xff]).collect(),
        png::ColorType::GrayscaleAlpha => buf
            .chunks_exact(2)
            .flat_map(|p| [p[0], p[0], p[0], p[1]])
            .collect(),
        // Indexed is expanded by the decoder when the transform is set; without
        // it we can't map palette indices to colors here, so decline.
        png::ColorType::Indexed => return None,
    };
    if rgba.len() < (w * h * 4) as usize {
        return None;
    }
    Some((w, h, rgba))
}

struct Tex {
    bind_group: wgpu::BindGroup,
    w: u32,
    h: u32,
}

pub struct ImageLayer {
    pipeline: wgpu::RenderPipeline,
    bgl: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniform: wgpu::Buffer,
    uniform_slots: u64,
    textures: HashMap<u32, Tex>,
    screen: [f32; 4], // xy = size, zw = origin
}

impl ImageLayer {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("image"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/image.wgsl").into()),
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("image-sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("image-bgl"),
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
                        has_dynamic_offset: true,
                        min_binding_size: wgpu::BufferSize::new(32),
                    },
                    count: None,
                },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("image-pl"),
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("image-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs",
                compilation_options: Default::default(),
                buffers: &[],
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

        let uniform_slots = 16;
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("image-uniform"),
            size: uniform_slots * SLOT,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        ImageLayer {
            pipeline,
            bgl,
            sampler,
            uniform,
            uniform_slots,
            textures: HashMap::new(),
            screen: [1.0, 1.0, 0.0, 0.0],
        }
    }

    /// Set the target size and grid origin (must match the cell renderer).
    pub fn set_screen(&mut self, w: f32, h: f32, ox: f32, oy: f32) {
        self.screen = [w, h, ox, oy];
    }

    /// Draw `quads` over the existing contents of `view` (does not clear).
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        quads: &[ImageQuad],
    ) {
        // Evict textures whose image is no longer placed.
        let live: std::collections::HashSet<u32> = quads.iter().map(|q| q.id).collect();
        self.textures.retain(|id, _| live.contains(id));

        if quads.is_empty() {
            return;
        }

        // Grow the uniform buffer if more images than slots this frame.
        if quads.len() as u64 > self.uniform_slots {
            self.uniform_slots = (quads.len() as u64).next_power_of_two();
            self.uniform = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("image-uniform"),
                size: self.uniform_slots * SLOT,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.textures.clear(); // bind groups reference the old buffer
        }

        for (i, q) in quads.iter().enumerate() {
            self.ensure_texture(device, queue, q);
            let u: [f32; 8] = [
                self.screen[0],
                self.screen[1],
                self.screen[2],
                self.screen[3],
                q.x,
                q.y,
                q.w,
                q.h,
            ];
            queue.write_buffer(&self.uniform, i as u64 * SLOT, bytemuck::cast_slice(&u));
        }

        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("image-frame"),
        });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("images"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load, // keep the rendered cells
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            pass.set_pipeline(&self.pipeline);
            for (i, q) in quads.iter().enumerate() {
                let Some(tex) = self.textures.get(&q.id) else {
                    continue;
                };
                pass.set_bind_group(0, &tex.bind_group, &[(i as u64 * SLOT) as u32]);
                pass.draw(0..6, 0..1);
            }
        }
        queue.submit(Some(enc.finish()));
    }

    /// Upload an image's texture on first use (cached by id; re-uploaded if the
    /// cached size differs, i.e. the id was reused for a different image).
    fn ensure_texture(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, q: &ImageQuad) {
        if let Some(t) = self.textures.get(&q.id) {
            if t.w == q.src_w && t.h == q.src_h {
                return;
            }
        }
        if q.src_w == 0 || q.src_h == 0 || q.rgba.len() < (q.src_w * q.src_h * 4) as usize {
            return;
        }
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("image-tex"),
            size: wgpu::Extent3d {
                width: q.src_w,
                height: q.src_h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &q.rgba[..(q.src_w * q.src_h * 4) as usize],
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(q.src_w * 4),
                rows_per_image: Some(q.src_h),
            },
            wgpu::Extent3d {
                width: q.src_w,
                height: q.src_h,
                depth_or_array_layers: 1,
            },
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("image-bg"),
            layout: &self.bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &self.uniform,
                        offset: 0,
                        size: wgpu::BufferSize::new(32),
                    }),
                },
            ],
        });
        self.textures.insert(
            q.id,
            Tex {
                bind_group,
                w: q.src_w,
                h: q.src_h,
            },
        );
    }
}
