use std::collections::HashMap;
use ab_glyph::{Font, FontRef, PxScale, ScaleFont};
use wgpu::util::DeviceExt;
use crate::quad_pipeline::CameraUniforms;

const ATLAS_SIZE: u32 = 2048;
const RASTER_HEIGHT: f32 = 128.0;
const PAD: f32 = 2.0;
const MAX_QUADS: u32 = 4096;

#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct TextVertex {
    position: [f32; 2],
    uv: [f32; 2],
    color: [f32; 4],
}

struct GlyphInfo {
    uv: [f32; 4],
    width: f32,
    height: f32,
    offset_x: f32,
    offset_y: f32,
    advance: f32,
    has_bitmap: bool,
}

pub struct TextPipeline {
    render_pipeline: wgpu::RenderPipeline,
    camera_buffer: wgpu::Buffer,
    camera_bind_group: wgpu::BindGroup,
    atlas_bind_group: wgpu::BindGroup,
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    vertices: Vec<TextVertex>,
    glyphs: HashMap<char, GlyphInfo>,
    draw_count: u32,
}

fn load_font_bytes() -> Option<Vec<u8>> {
    let paths = [
        "/usr/share/fonts/gdouros-symbola/Symbola.ttf",
        "/usr/share/fonts/dejavu-sans-fonts/DejaVuSans.ttf",
        "/usr/share/fonts/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "assets/fonts/Symbola.ttf",
        "assets/fonts/DejaVuSans.ttf",
    ];
    for p in paths {
        if let Ok(bytes) = std::fs::read(p) {
            log::info!("Loaded font from {}", p);
            return Some(bytes);
        }
    }
    None
}

fn piece_char(kind: chess::PieceKind) -> char {
    match kind {
        chess::PieceKind::King => '\u{265A}',
        chess::PieceKind::Queen => '\u{265B}',
        chess::PieceKind::Rook => '\u{265C}',
        chess::PieceKind::Bishop => '\u{265D}',
        chess::PieceKind::Knight => '\u{265E}',
        chess::PieceKind::Pawn => '\u{265F}',
    }
}

fn piece_colors(color: chess::Color) -> ([f32; 4], [f32; 4]) {
    let shadow = [0.0, 0.0, 0.0, 0.35];
    match color {
        chess::Color::White => ([0.98, 0.98, 0.98, 1.0], shadow),
        chess::Color::Black => ([0.10, 0.10, 0.10, 1.0], shadow),
    }
}

impl TextPipeline {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, format: wgpu::TextureFormat, width: f32, height: f32) -> Self {
        let font_bytes = load_font_bytes()
            .expect("No font with chess glyphs found. Install a Unicode font or place one in assets/fonts/");
        let font = FontRef::try_from_slice(&font_bytes).expect("Failed to parse font");
        let scale = PxScale::from(RASTER_HEIGHT);
        let scaled = font.as_scaled(scale);

        // ASCII printable range + the filled chess glyphs.
        let mut chars: Vec<char> = (32u8..=126u8).map(|b| b as char).collect();
        for ch in ['\u{265A}', '\u{265B}', '\u{265C}', '\u{265D}', '\u{265E}', '\u{265F}'] {
            chars.push(ch);
        }

        let mut atlas_data = vec![0u8; (ATLAS_SIZE * ATLAS_SIZE) as usize];
        let mut glyphs = HashMap::new();
        let mut pen_x: f32 = PAD;
        let mut pen_y: f32 = PAD;
        let mut row_height: f32 = 0.0;

        for ch in chars {
            let glyph_id = font.glyph_id(ch);
            let advance = scaled.h_advance(glyph_id);
            let glyph = glyph_id.with_scale(scale);

            if let Some(outlined) = font.outline_glyph(glyph) {
                let bounds = outlined.px_bounds();
                let gw = bounds.max.x - bounds.min.x;
                let gh = bounds.max.y - bounds.min.y;

                if pen_x + gw + PAD > ATLAS_SIZE as f32 {
                    pen_x = PAD;
                    pen_y += row_height + PAD;
                    row_height = 0.0;
                }
                row_height = row_height.max(gh);

                outlined.draw(|x, y, c| {
                    let ax = (pen_x + x as f32) as usize;
                    let ay = (pen_y + y as f32) as usize;
                    if ax < ATLAS_SIZE as usize && ay < ATLAS_SIZE as usize {
                        let idx = ay * ATLAS_SIZE as usize + ax;
                        let v = (c * 255.0) as u8;
                        if v > atlas_data[idx] {
                            atlas_data[idx] = v;
                        }
                    }
                });

                glyphs.insert(ch, GlyphInfo {
                    uv: [
                        pen_x / ATLAS_SIZE as f32,
                        pen_y / ATLAS_SIZE as f32,
                        (pen_x + gw) / ATLAS_SIZE as f32,
                        (pen_y + gh) / ATLAS_SIZE as f32,
                    ],
                    width: gw,
                    height: gh,
                    offset_x: bounds.min.x,
                    offset_y: bounds.min.y,
                    advance,
                    has_bitmap: true,
                });
                pen_x += gw + PAD;
            } else {
                // No outline (e.g. space): record advance only.
                glyphs.insert(ch, GlyphInfo {
                    uv: [0.0; 4],
                    width: 0.0,
                    height: 0.0,
                    offset_x: 0.0,
                    offset_y: 0.0,
                    advance,
                    has_bitmap: false,
                });
            }
        }

        let atlas_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("glyph_atlas"),
            size: wgpu::Extent3d { width: ATLAS_SIZE, height: ATLAS_SIZE, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            atlas_texture.as_image_copy(),
            &atlas_data,
            wgpu::ImageDataLayout { offset: 0, bytes_per_row: Some(ATLAS_SIZE), rows_per_image: Some(ATLAS_SIZE) },
            wgpu::Extent3d { width: ATLAS_SIZE, height: ATLAS_SIZE, depth_or_array_layers: 1 },
        );
        let atlas_view = atlas_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Nearest,
            lod_min_clamp: 0.0,
            lod_max_clamp: 32.0,
            compare: None,
            anisotropy_clamp: 1,
            border_color: None,
            label: Some("atlas_sampler"),
        });

        let camera_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("text camera buffer"),
            size: std::mem::size_of::<CameraUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let camera_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
            label: Some("text_camera_bgl"),
        });
        let camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            layout: &camera_bgl,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: camera_buffer.as_entire_binding() }],
            label: Some("text_camera_bg"),
        });

        let atlas_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
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
            ],
            label: Some("atlas_bgl"),
        });
        let atlas_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            layout: &atlas_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&atlas_view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&sampler) },
            ],
            label: Some("atlas_bg"),
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("text shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("text_shader.wgsl").into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("text pipeline layout"),
            bind_group_layouts: &[&camera_bgl, &atlas_bgl],
            push_constant_ranges: &[],
        });
        let render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("text pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<TextVertex>() as wgpu::BufferAddress,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[
                        wgpu::VertexAttribute { offset: 0, shader_location: 0, format: wgpu::VertexFormat::Float32x2 },
                        wgpu::VertexAttribute { offset: 8, shader_location: 1, format: wgpu::VertexFormat::Float32x2 },
                        wgpu::VertexAttribute { offset: 16, shader_location: 2, format: wgpu::VertexFormat::Float32x4 },
                    ],
                }],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState {
                        color: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::SrcAlpha,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                        alpha: wgpu::BlendComponent {
                            src_factor: wgpu::BlendFactor::One,
                            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                            operation: wgpu::BlendOperation::Add,
                        },
                    }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                polygon_mode: wgpu::PolygonMode::Fill,
                unclipped_depth: false,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState { count: 1, mask: !0, alpha_to_coverage_enabled: false },
            multiview: None,
            cache: None,
        });

        let vertex_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("text vertex buffer"),
            size: (MAX_QUADS * 4 * std::mem::size_of::<TextVertex>() as u32) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut indices = Vec::with_capacity((MAX_QUADS * 6) as usize);
        for i in 0..MAX_QUADS {
            let base = i * 4;
            indices.push(base); indices.push(base + 1); indices.push(base + 2);
            indices.push(base); indices.push(base + 2); indices.push(base + 3);
        }
        let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("text index buffer"),
            contents: bytemuck::cast_slice(&indices),
            usage: wgpu::BufferUsages::INDEX,
        });

        let mut pipeline = Self {
            render_pipeline,
            camera_buffer,
            camera_bind_group,
            atlas_bind_group,
            vertex_buffer,
            index_buffer,
            vertices: Vec::with_capacity((MAX_QUADS * 4) as usize),
            glyphs,
            draw_count: 0,
        };
        pipeline.resize(queue, width, height);
        pipeline
    }

    pub fn resize(&mut self, queue: &wgpu::Queue, width: f32, height: f32) {
        let projection = glam::Mat4::orthographic_rh(0.0, width, height, 0.0, -1.0, 1.0);
        let cam = CameraUniforms { view_proj: projection.to_cols_array_2d() };
        queue.write_buffer(&self.camera_buffer, 0, bytemuck::cast_slice(&[cam]));
    }

    fn measure_text(&self, text: &str, scale: f32) -> f32 {
        let mut w = 0.0;
        for ch in text.chars() {
            if let Some(g) = self.glyphs.get(&ch) {
                w += g.advance * scale;
            }
        }
        w
    }

    pub fn prepare(
        &mut self,
        queue: &wgpu::Queue,
        board: &chess::Board,
        offset_x: f32,
        offset_y: f32,
        sq: f32,
        banner: Option<&str>,
        window_w: f32,
        window_h: f32,
    ) {
        self.vertices.clear();

        // Pieces
        if sq > 0.0 {
            for (square, piece) in board.iter() {
                let file = square.file() as f32;
                let rank = square.rank() as f32;
                let cx = offset_x + file * sq + sq * 0.5;
                let cy = offset_y + (7.0 - rank) * sq + sq * 0.5;
                let ch = piece_char(piece.kind);
                let glyph = self.glyphs.get(&ch).map(|g| (g.uv, g.width, g.height));
                if let Some((uv, gw, gh)) = glyph {
                    let scale = (sq * 0.82) / RASTER_HEIGHT;
                    let w = gw * scale;
                    let h = gh * scale;
                    let x0 = cx - w * 0.5;
                    let y0 = cy - h * 0.5;
                    let (tint, shadow) = piece_colors(piece.color);
                    let sdx = sq * 0.02;
                    let sdy = sq * 0.03;
                    self.push_quad(x0 + sdx, y0 + sdy, w, h, uv, shadow);
                    self.push_quad(x0, y0, w, h, uv, tint);
                }
            }
        }

        // Game-over banner (centered, with shadow for readability)
        if let Some(text) = banner {
            let size = (sq * 1.1).max(28.0);
            let scale = size / RASTER_HEIGHT;
            let total_w = self.measure_text(text, scale);
            let x = window_w / 2.0 - total_w / 2.0;
            let baseline = window_h / 2.0 + size * 0.35;
            let off = size * 0.04;
            self.push_text(text, x + off, baseline + off, scale, [0.0, 0.0, 0.0, 0.85]);
            self.push_text(text, x, baseline, scale, [1.0, 1.0, 1.0, 1.0]);
        }

        self.draw_count = (self.vertices.len() / 4) as u32;
        if !self.vertices.is_empty() {
            queue.write_buffer(&self.vertex_buffer, 0, bytemuck::cast_slice(&self.vertices));
        }
    }

    fn push_text(&mut self, text: &str, x: f32, baseline_y: f32, scale: f32, color: [f32; 4]) {
        let mut pen_x = x;
        for ch in text.chars() {
            // Copy the glyph data out so the immutable borrow of self.glyphs
            // ends before we mutably borrow self to push vertices.
            let glyph = self.glyphs.get(&ch).map(|g| {
                (g.uv, g.width, g.height, g.offset_x, g.offset_y, g.advance, g.has_bitmap)
            });
            if let Some((uv, gw, gh, ox, oy, advance, has_bitmap)) = glyph {
                if has_bitmap {
                    let gx = pen_x + ox * scale;
                    let gy = baseline_y + oy * scale;
                    let w = gw * scale;
                    let h = gh * scale;
                    self.push_quad(gx, gy, w, h, uv, color);
                }
                pen_x += advance * scale;
            }
        }
    }

    fn push_quad(&mut self, x: f32, y: f32, w: f32, h: f32, uv: [f32; 4], color: [f32; 4]) {
        self.vertices.push(TextVertex { position: [x, y], uv: [uv[0], uv[1]], color });
        self.vertices.push(TextVertex { position: [x + w, y], uv: [uv[2], uv[1]], color });
        self.vertices.push(TextVertex { position: [x + w, y + h], uv: [uv[2], uv[3]], color });
        self.vertices.push(TextVertex { position: [x, y + h], uv: [uv[0], uv[3]], color });
    }

    pub fn draw<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) {
        if self.draw_count == 0 {
            return;
        }
        pass.set_pipeline(&self.render_pipeline);
        pass.set_bind_group(0, &self.camera_bind_group, &[]);
        pass.set_bind_group(1, &self.atlas_bind_group, &[]);
        pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
        pass.set_index_buffer(self.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..self.draw_count * 6, 0, 0..1);
    }
}
