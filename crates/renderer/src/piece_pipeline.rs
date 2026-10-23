//! Piece rendering: the cburnett SVG set rasterized once at init into a
//! GPU texture atlas, drawn as textured quads. Replaces the old glyph-based
//! piece path (no tint, no stamped outline — the SVG carries its own colors
//! and contrast).

use shakmaty::{Color, Position, Role};
use wgpu::util::DeviceExt;

use crate::quad_pipeline::CameraUniforms;
use crate::PieceElement;

/// Raster size of one piece cell in the atlas; sharp enough for HiDPI.
const PIECE_RASTER: u32 = 256;
/// 6 roles × 2 colours fit a 4×3 grid of cells.
const ATLAS_COLS: u32 = 4;
const ATLAS_ROWS: u32 = 3;
const MAX_QUADS: u32 = 4096;

#[repr(C)]
#[derive(Copy, Clone, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct PieceVertex {
    position: [f32; 2],
    uv: [f32; 2],
}

/// Atlas slot of a piece: white roles first, then black.
fn piece_index(role: Role, color: Color) -> usize {
    let role_idx = match role {
        Role::Pawn => 0,
        Role::Knight => 1,
        Role::Bishop => 2,
        Role::Rook => 3,
        Role::Queen => 4,
        Role::King => 5,
    };
    match color {
        Color::White => role_idx,
        Color::Black => role_idx + 6,
    }
}

/// The embedded cburnett SVG for a piece, selected by (role, color).
fn piece_svg(role: Role, color: Color) -> &'static str {
    match (color, role) {
        (Color::White, Role::King) => include_str!("../assets/cburnett/wK.svg"),
        (Color::White, Role::Queen) => include_str!("../assets/cburnett/wQ.svg"),
        (Color::White, Role::Rook) => include_str!("../assets/cburnett/wR.svg"),
        (Color::White, Role::Bishop) => include_str!("../assets/cburnett/wB.svg"),
        (Color::White, Role::Knight) => include_str!("../assets/cburnett/wN.svg"),
        (Color::White, Role::Pawn) => include_str!("../assets/cburnett/wP.svg"),
        (Color::Black, Role::King) => include_str!("../assets/cburnett/bK.svg"),
        (Color::Black, Role::Queen) => include_str!("../assets/cburnett/bQ.svg"),
        (Color::Black, Role::Rook) => include_str!("../assets/cburnett/bR.svg"),
        (Color::Black, Role::Bishop) => include_str!("../assets/cburnett/bB.svg"),
        (Color::Black, Role::Knight) => include_str!("../assets/cburnett/bN.svg"),
        (Color::Black, Role::Pawn) => include_str!("../assets/cburnett/bP.svg"),
    }
}

/// Rasterize one piece SVG to straight-alpha RGBA8 bytes (PIECE_RASTER²).
/// Pure CPU work, a few milliseconds for all twelve — done once at init.
pub fn rasterize_piece(role: Role, color: Color) -> Result<Vec<u8>, String> {
    let svg = piece_svg(role, color);
    let tree = resvg::usvg::Tree::from_str(svg, &resvg::usvg::Options::default())
        .map_err(|e| format!("{}{} svg parse failed: {e}", color_name(color), role_name(role)))?;

    let size = tree.size();
    let sx = PIECE_RASTER as f32 / size.width();
    let sy = PIECE_RASTER as f32 / size.height();

    let mut pixmap = resvg::tiny_skia::Pixmap::new(PIECE_RASTER, PIECE_RASTER)
        .ok_or_else(|| format!("{}{} raster alloc failed", color_name(color), role_name(role)))?;
    resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(sx, sy), &mut pixmap.as_mut());

    // tiny-skia hands us premultiplied sRGBA; the blend we sample with
    // expects straight alpha, so demultiply before upload (private color
    // types make it easier over the raw bytes). Premultiplied components
    // never exceed alpha, so the checked division always lands.
    for px in pixmap.data_mut().as_chunks_mut::<4>().0.iter_mut() {
        let a = px[3] as u32;
        for c in px[..3].iter_mut() {
            *c = (*c as u32)
                .checked_mul(255)
                .and_then(|v| v.checked_add(a / 2))
                .and_then(|v| v.checked_div(a))
                .map(|v| v.min(255))
                .unwrap_or(255) as u8;
        }
    }
    Ok(pixmap.data().to_vec())
}

fn color_name(color: Color) -> &'static str {
    match color {
        Color::White => "white ",
        Color::Black => "black ",
    }
}

fn role_name(role: Role) -> &'static str {
    match role {
        Role::King => "king",
        Role::Queen => "queen",
        Role::Rook => "rook",
        Role::Bishop => "bishop",
        Role::Knight => "knight",
        Role::Pawn => "pawn",
    }
}

pub struct PiecePipeline {
    render_pipeline: wgpu::RenderPipeline,
    camera_buffer: wgpu::Buffer,
    camera_bind_group: wgpu::BindGroup,
    atlas_bind_group: wgpu::BindGroup,
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    vertices: Vec<PieceVertex>,
    draw_count: u32,
}

impl PiecePipeline {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, format: wgpu::TextureFormat, width: f32, height: f32) -> Self {
        // Rasterize all 12 pieces and pack them into one atlas.
        let atlas_size = (ATLAS_COLS * PIECE_RASTER, ATLAS_ROWS * PIECE_RASTER);
        let mut atlas = vec![0u8; (atlas_size.0 * atlas_size.1 * 4) as usize];
        for idx in 0..(ATLAS_COLS * ATLAS_ROWS) as usize {
            let role = match idx % 6 {
                0 => Role::Pawn,
                1 => Role::Knight,
                2 => Role::Bishop,
                3 => Role::Rook,
                4 => Role::Queen,
                _ => Role::King,
            };
            let color = if idx < 6 { Color::White } else { Color::Black };
            let pixels = rasterize_piece(role, color)
                .unwrap_or_else(|e| panic!("piece rasterization failed: {e}"));
            let col = (idx as u32 % ATLAS_COLS) * PIECE_RASTER;
            let row = (idx as u32 / ATLAS_COLS) * PIECE_RASTER;
            for y in 0..PIECE_RASTER {
                let src = (y * PIECE_RASTER * 4) as usize;
                let dst = ((row + y) * atlas_size.0 + col) as usize * 4;
                atlas[dst..dst + (PIECE_RASTER * 4) as usize]
                    .copy_from_slice(&pixels[src..src + (PIECE_RASTER * 4) as usize]);
            }
        }

        let atlas_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("piece_atlas"),
            size: wgpu::Extent3d {
                width: atlas_size.0,
                height: atlas_size.1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            atlas_texture.as_image_copy(),
            &atlas,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(atlas_size.0 * 4),
                rows_per_image: Some(atlas_size.1),
            },
            wgpu::Extent3d {
                width: atlas_size.0,
                height: atlas_size.1,
                depth_or_array_layers: 1,
            },
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
            lod_max_clamp: 1.0,
            compare: None,
            anisotropy_clamp: 1,
            border_color: None,
            label: Some("piece_sampler"),
        });

        let camera_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("piece camera buffer"),
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
            label: Some("piece_camera_bgl"),
        });
        let camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            layout: &camera_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buffer.as_entire_binding(),
            }],
            label: Some("piece_camera_bg"),
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
            label: Some("piece_atlas_bgl"),
        });
        let atlas_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            layout: &atlas_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&atlas_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
            label: Some("piece_atlas_bg"),
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("piece shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("piece_shader.wgsl").into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("piece pipeline layout"),
            bind_group_layouts: &[&camera_bgl, &atlas_bgl],
            push_constant_ranges: &[],
        });
        let render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("piece pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<PieceVertex>() as wgpu::BufferAddress,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[
                        wgpu::VertexAttribute { offset: 0, shader_location: 0, format: wgpu::VertexFormat::Float32x2 },
                        wgpu::VertexAttribute { offset: 8, shader_location: 1, format: wgpu::VertexFormat::Float32x2 },
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
            label: Some("piece vertex buffer"),
            size: (MAX_QUADS * 4 * std::mem::size_of::<PieceVertex>() as u32) as u64,
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
            label: Some("piece index buffer"),
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

    /// Upload the whole piece scene (board pieces + extras such as the
    /// promotion menu, plus the piece currently under the cursor).
    // Parameter list mirrors the board part of `Scene`; flat by design.
    #[allow(clippy::too_many_arguments)]
    pub fn prepare(
        &mut self,
        queue: &wgpu::Queue,
        board: &shakmaty::variant::VariantPosition,
        offset_x: f32,
        offset_y: f32,
        sq: f32,
        pieces: &[PieceElement],
        dragged: Option<(shakmaty::Square, PieceElement)>,
    ) {
        self.vertices.clear();

        // The dragged piece's origin square is skipped, so the piece exists
        // only under the cursor while it is being moved.
        let origin = dragged.map(|(from, _)| from);
        if sq > 0.0 {
            for square in shakmaty::Square::ALL {
                if origin == Some(square) {
                    continue;
                }
                let Some(piece) = board.board().piece_at(square) else {
                    continue;
                };
                let idx = square as u8;
                let file = (idx % 8) as f32;
                let rank = (idx / 8) as f32;
                let cx = offset_x + file * sq + sq * 0.5;
                let cy = offset_y + (7.0 - rank) * sq + sq * 0.5;
                self.push_piece(piece.role, piece.color, cx, cy, sq);
            }
        }

        for p in pieces {
            self.push_piece(p.role, p.color, p.x, p.y, p.size);
        }

        // Last, so the dragged piece is on top of every other piece (only the
        // text pass, i.e. the game-over banner, draws after this pipeline).
        if let Some((_, p)) = dragged {
            self.push_piece(p.role, p.color, p.x, p.y, p.size);
        }

        self.draw_count = (self.vertices.len() / 4) as u32;
        if !self.vertices.is_empty() {
            queue.write_buffer(&self.vertex_buffer, 0, bytemuck::cast_slice(&self.vertices));
        }
    }

    /// A piece occupies its full square, centred; the SVG's own padding
    /// keeps it visually balanced, like Lichess's own board.
    fn push_piece(&mut self, role: Role, color: Color, cx: f32, cy: f32, size: f32) {
        let idx = piece_index(role, color);
        // Half-texel inset on every cell edge keeps linear filtering from
        // bleeding into the neighbouring atlas cell.
        let inset = 0.5;
        let cell_w = 1.0 / ATLAS_COLS as f32;
        let cell_h = 1.0 / ATLAS_ROWS as f32;
        let u0 = (idx as u32 % ATLAS_COLS) as f32 * cell_w
            + inset / PIECE_RASTER as f32;
        let v0 = (idx as u32 / ATLAS_COLS) as f32 * cell_h
            + inset / PIECE_RASTER as f32;
        let u1 = u0 + cell_w - 2.0 * inset / PIECE_RASTER as f32;
        let v1 = v0 + cell_h - 2.0 * inset / PIECE_RASTER as f32;

        let x0 = cx - size * 0.5;
        let y0 = cy - size * 0.5;
        self.vertices.push(PieceVertex { position: [x0, y0], uv: [u0, v0] });
        self.vertices.push(PieceVertex { position: [x0 + size, y0], uv: [u1, v0] });
        self.vertices.push(PieceVertex { position: [x0 + size, y0 + size], uv: [u1, v1] });
        self.vertices.push(PieceVertex { position: [x0, y0 + size], uv: [u0, v1] });
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

#[cfg(test)]
mod tests {
    use super::*;

    /// All 12 piece SVGs must load, rasterize, and produce a visibly non-empty
    /// silhouette at the raster size.
    #[test]
    fn all_twelve_pieces_rasterize() {
        for role in [Role::Pawn, Role::Knight, Role::Bishop, Role::Rook, Role::Queen, Role::King] {
            for color in [Color::White, Color::Black] {
                let pixels = rasterize_piece(role, color)
                    .unwrap_or_else(|e| panic!("{}{} failed: {e}", color_name(color), role_name(role)));
                assert_eq!(pixels.len(), (PIECE_RASTER * PIECE_RASTER * 4) as usize);
                let opaque = pixels.as_chunks::<4>().0.iter().filter(|p| p[3] > 128).count();
                // A piece silhouette fills well over 5% of its cell.
                assert!(
                    opaque as f32 > 0.05 * (PIECE_RASTER * PIECE_RASTER) as f32,
                    "{:?} {:?} silhouette too small: {} px",
                    color, role, opaque
                );
            }
        }
    }

    /// Every (role, color) pair maps to a distinct atlas slot.
    #[test]
    fn piece_indexes_are_unique() {
        use std::collections::HashSet;
        let mut seen = HashSet::new();
        for role in [Role::Pawn, Role::Knight, Role::Bishop, Role::Rook, Role::Queen, Role::King] {
            for color in [Color::White, Color::Black] {
                assert!(seen.insert(piece_index(role, color)), "duplicate slot for {color:?} {role:?}");
            }
        }
        assert_eq!(seen.len(), 12);
    }
}
