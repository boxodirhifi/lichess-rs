use std::sync::Arc;
use winit::window::Window;
use winit::dpi::PhysicalSize;
use shakmaty::variant::VariantPosition;
use shakmaty::{Color, Role};

mod quad_pipeline;
mod text_pipeline;
pub use quad_pipeline::Quad;
pub use text_pipeline::{TextAlign, TextElement};
use quad_pipeline::QuadPipeline;
use text_pipeline::TextPipeline;

/// A single piece drawn at an arbitrary screen position, using the same glyph
/// and shadow treatment as the pieces on the board.
#[derive(Debug, Clone, Copy)]
pub struct PieceElement {
    pub role: Role,
    pub color: Color,
    /// Center of the piece, in screen pixels.
    pub x: f32,
    pub y: f32,
    /// Nominal size in pixels; the glyph is scaled to ~0.82 of this, like a square.
    pub size: f32,
}

pub struct Scene<'a> {
    pub quads: &'a [Quad],
    pub board: &'a VariantPosition,
    pub board_offset: (f32, f32),
    pub square_size: f32,
    pub banner: Option<&'a str>,
    pub texts: &'a [TextElement],
    /// Extra pieces to draw (e.g. a promotion menu); drawn after the board pieces.
    pub pieces: &'a [PieceElement],
}

pub struct Renderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    #[allow(dead_code)]
    window: Arc<Window>,
    quad_pipeline: QuadPipeline,
    text_pipeline: TextPipeline,
}

impl Renderer {
    pub async fn new(window: Arc<Window>) -> Self {
        let size = window.inner_size();
        let safe_width = size.width.max(1);
        let safe_height = size.height.max(1);

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..Default::default()
        });
        let surface = instance.create_surface(window.clone()).expect("Failed to create surface");
        let adapter = instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::default(),
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }).await.expect("Failed to find a suitable GPU adapter");
        let (device, queue) = adapter.request_device(&wgpu::DeviceDescriptor {
            label: None,
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            memory_hints: wgpu::MemoryHints::Performance,
        }, None).await.expect("Failed to create logical device");

        let surface_caps = surface.get_capabilities(&adapter);
        let surface_format = surface_caps.formats.iter()
            .find(|f| f.is_srgb())
            .copied()
            .unwrap_or(surface_caps.formats[0]);

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: surface_format,
            width: safe_width,
            height: safe_height,
            present_mode: surface_caps.present_modes[0],
            alpha_mode: surface_caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        let quad_pipeline = QuadPipeline::new(&device, &queue, surface_format, safe_width as f32, safe_height as f32);
        let text_pipeline = TextPipeline::new(&device, &queue, surface_format, safe_width as f32, safe_height as f32);

        Self { surface, device, queue, config, window, quad_pipeline, text_pipeline }
    }

    pub fn resize(&mut self, new_size: PhysicalSize<u32>) {
        if new_size.width > 0 && new_size.height > 0 {
            self.config.width = new_size.width;
            self.config.height = new_size.height;
            self.surface.configure(&self.device, &self.config);
            self.quad_pipeline.resize(&self.queue, new_size.width as f32, new_size.height as f32);
            self.text_pipeline.resize(&self.queue, new_size.width as f32, new_size.height as f32);
        }
    }

    pub fn render(&mut self, scene: &Scene) {
        if self.config.width == 0 || self.config.height == 0 {
            return;
        }
        let output = match self.surface.get_current_texture() {
            Ok(tex) => tex,
            Err(wgpu::SurfaceError::Lost) => {
                self.surface.configure(&self.device, &self.config);
                return;
            }
            Err(wgpu::SurfaceError::OutOfMemory) => {
                log::error!("Out of GPU memory!");
                return;
            }
            Err(e) => {
                log::error!("Surface error: {:?}", e);
                return;
            }
        };
        let view = output.texture.create_view(&wgpu::TextureViewDescriptor::default());

        // Convert the banner (if any) into a centered TextElement using window size.
        let mut all_texts: Vec<TextElement> = scene.texts.to_vec();
        if let Some(banner) = scene.banner {
            let size = (scene.square_size * 1.1).max(28.0);
            all_texts.push(TextElement {
                text: banner.to_string(),
                x: self.config.width as f32 / 2.0,
                y: self.config.height as f32 / 2.0 + size * 0.35,
                size,
                color: [1.0, 1.0, 1.0, 1.0],
                align: TextAlign::Center,
                shadow: true,
            });
        }

        self.quad_pipeline.prepare(&self.queue, scene.quads);
        self.text_pipeline.prepare(
            &self.queue,
            scene.board,
            scene.board_offset.0,
            scene.board_offset.1,
            scene.square_size,
            scene.pieces,
            &all_texts,
        );

        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("Render Encoder"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Render Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color { r: 0.188, g: 0.180, b: 0.173, a: 1.0 }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
            });
            self.quad_pipeline.draw(&mut pass);
            self.text_pipeline.draw(&mut pass);
        }
        self.queue.submit(std::iter::once(encoder.finish()));
        output.present();
    }
}
