use std::sync::Arc;
use winit::window::Window;
use winit::dpi::PhysicalSize;
use shakmaty::variant::VariantPosition;
use shakmaty::{Color, Role, Square};

mod piece_pipeline;
mod quad_pipeline;
mod text_pipeline;
pub use piece_pipeline::{rasterize_piece, PieceSet};
pub use quad_pipeline::Quad;
pub use text_pipeline::{TextAlign, TextElement};
use piece_pipeline::PiecePipeline;
use quad_pipeline::QuadPipeline;
use text_pipeline::TextPipeline;

/// Long banners (error messages, not just "You win!") are wrapped and shrunk
/// so they always stay inside the window and remain readable.
const BANNER_MAX_LINES: usize = 4;
const BANNER_MIN_SIZE: f32 = 16.0;
const BANNER_MAX_SIZE: f32 = 64.0;

/// Greedy word wrap at `max_width`. A word longer than a whole line is broken
/// by character, so a pasted URL or status string cannot overflow.
fn wrap_banner(
    text: &str,
    max_width: f32,
    size: f32,
    measure: &mut impl FnMut(&str, f32) -> f32,
) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let candidate = if current.is_empty() {
            word.to_string()
        } else {
            format!("{current} {word}")
        };
        if measure(&candidate, size) <= max_width {
            current = candidate;
            continue;
        }
        if !current.is_empty() {
            lines.push(std::mem::take(&mut current));
        }
        if measure(word, size) <= max_width {
            current = word.to_string();
        } else {
            let mut chunk = String::new();
            for ch in word.chars() {
                let mut next = chunk.clone();
                next.push(ch);
                if measure(&next, size) > max_width && !chunk.is_empty() {
                    lines.push(std::mem::take(&mut chunk));
                }
                chunk.push(ch);
            }
            current = chunk;
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// Cut `text` to whatever fits at `size`, always marking it with `...`.
fn ellipsize(text: &str, max_width: f32, size: f32, measure: &mut impl FnMut(&str, f32) -> f32) -> String {
    let suffix = "...";
    let mut out = String::new();
    for ch in text.chars() {
        let mut candidate = out.clone();
        candidate.push(ch);
        candidate.push_str(suffix);
        if measure(&candidate, size) > max_width {
            break;
        }
        out.push(ch);
    }
    out.push_str(suffix);
    out
}

/// Shrink until the wrapped text fits [`BANNER_MAX_LINES`], then truncate with
/// an ellipsis as a last resort. Returns the lines and the font size used.
fn fit_banner(
    text: &str,
    max_width: f32,
    start_size: f32,
    mut measure: impl FnMut(&str, f32) -> f32,
) -> (Vec<String>, f32) {
    let mut size = start_size.clamp(BANNER_MIN_SIZE, BANNER_MAX_SIZE);
    loop {
        let mut lines = wrap_banner(text, max_width, size, &mut measure);
        if lines.len() <= BANNER_MAX_LINES {
            return (lines, size);
        }
        if size <= BANNER_MIN_SIZE {
            lines.truncate(BANNER_MAX_LINES);
            if let Some(last) = lines.last_mut() {
                *last = ellipsize(last, max_width, size, &mut measure);
            }
            return (lines, size);
        }
        size = (size - 2.0).max(BANNER_MIN_SIZE);
    }
}

/// A single piece drawn at an arbitrary screen position (e.g. the promotion
/// menu or a Crazyhouse pocket). Board pieces come from the position itself;
/// both go through the same SVG-atlas textured-quad path.
#[derive(Debug, Clone, Copy)]
pub struct PieceElement {
    pub role: Role,
    pub color: Color,
    /// Center of the piece, in screen pixels.
    pub x: f32,
    pub y: f32,
    /// Edge length in pixels; the piece fills its square.
    pub size: f32,
    /// Straight-alpha multiplier; 1.0 is the normal SVG opacity, pocket pieces
    /// that cannot be dropped use ~0.5 to read as disabled.
    pub alpha: f32,
}

/// Which part of the UI a dragged piece was lifted from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DragSource {
    /// A board square: its piece is hidden while the drag runs.
    Board(Square),
    /// A Crazyhouse pocket: nothing on the board is hidden.
    Pocket,
}

pub struct Scene<'a> {
    pub quads: &'a [Quad],
    /// Quads drawn after the piece pass, so modal panels (the challenge
    /// dialog) cover the board instead of sitting underneath it. Text is
    /// still painted last, on top of everything.
    pub overlay_quads: &'a [Quad],
    pub board: &'a VariantPosition,
    pub board_offset: (f32, f32),
    pub square_size: f32,
    pub banner: Option<&'a str>,
    pub texts: &'a [TextElement],
    /// Extra pieces to draw (e.g. a promotion menu); drawn after the board pieces.
    pub pieces: &'a [PieceElement],
    /// A piece being dragged: where it came from, plus the element to draw
    /// under the cursor. Drawn last, so it floats above everything else.
    pub dragged: Option<(DragSource, PieceElement)>,
}

pub struct Renderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    #[allow(dead_code)]
    window: Arc<Window>,
    quad_pipeline: QuadPipeline,
    overlay_pipeline: QuadPipeline,
    piece_pipeline: PiecePipeline,
    text_pipeline: TextPipeline,
}

impl Renderer {
    /// Build the renderer with `pieces` as the initial SVG set; config
    /// hot-reload swaps it later through [`Renderer::reload_pieces`].
    pub async fn new(window: Arc<Window>, pieces: PieceSet) -> Self {
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
        // Same geometry pipeline, drawn after pieces: modal panels must cover
        // the board rather than being hidden behind the piece quads.
        let overlay_pipeline = QuadPipeline::new(&device, &queue, surface_format, safe_width as f32, safe_height as f32);
        // SVG piece atlas: 12 SVGs of the configured set rasterized here, on
        // the main thread, in milliseconds.
        let piece_pipeline = PiecePipeline::new(
            &device,
            &queue,
            surface_format,
            safe_width as f32,
            safe_height as f32,
            pieces,
        );
        let text_pipeline = TextPipeline::new(&device, &queue, surface_format, safe_width as f32, safe_height as f32);

        Self {
            surface,
            device,
            queue,
            config,
            window,
            quad_pipeline,
            overlay_pipeline,
            piece_pipeline,
            text_pipeline,
        }
    }

    pub fn resize(&mut self, new_size: PhysicalSize<u32>) {
        if new_size.width > 0 && new_size.height > 0 {
            self.config.width = new_size.width;
            self.config.height = new_size.height;
            self.surface.configure(&self.device, &self.config);
            self.quad_pipeline.resize(&self.queue, new_size.width as f32, new_size.height as f32);
            self.overlay_pipeline.resize(&self.queue, new_size.width as f32, new_size.height as f32);
            self.piece_pipeline.resize(&self.queue, new_size.width as f32, new_size.height as f32);
            self.text_pipeline.resize(&self.queue, new_size.width as f32, new_size.height as f32);
        }
    }

    /// Swap the SVG piece set at runtime and re-rasterize the atlas. Returns
    /// `false` (keeping the current set) for unknown names or raster failures.
    pub fn reload_pieces(&mut self, set_name: &str) -> bool {
        let Some(set) = PieceSet::from_name(set_name) else {
            log::warn!("Unknown piece set {set_name:?}; keeping the current one");
            return false;
        };
        self.piece_pipeline.reload(&self.queue, set)
    }

    /// Width in pixels of `text` at `size`, using the same glyph advances as
    /// rendering. Clients use this to keep extra UI rows clear of text.
    pub fn text_width(&self, text: &str, size: f32) -> f32 {
        self.text_pipeline.text_width(text, size)
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

        // Convert the banner (if any) into centered TextElement(s) using window
        // size. Long messages wrap and shrink to fit instead of running off
        // both sides of the window.
        let mut all_texts: Vec<TextElement> = scene.texts.to_vec();
        if let Some(banner) = scene.banner {
            let max_width = (self.config.width as f32 * 0.86).max(80.0);
            let start = (scene.square_size * 1.1).max(28.0);
            let (lines, size) = fit_banner(banner, max_width, start, |text, size| {
                self.text_pipeline.text_width(text, size)
            });
            let line_height = size * 1.2;
            let block_height = line_height * (lines.len().saturating_sub(1)) as f32;
            let first_y = self.config.height as f32 / 2.0 + size * 0.35 - block_height / 2.0;
            for (index, line) in lines.into_iter().enumerate() {
                all_texts.push(TextElement {
                    text: line,
                    x: self.config.width as f32 / 2.0,
                    y: first_y + index as f32 * line_height,
                    size,
                    color: [1.0, 1.0, 1.0, 1.0],
                    align: TextAlign::Center,
                    shadow: true,
                });
            }
        }

        self.quad_pipeline.prepare(&self.queue, scene.quads);
        self.overlay_pipeline.prepare(&self.queue, scene.overlay_quads);
        self.piece_pipeline.prepare(
            &self.queue,
            scene.board,
            scene.board_offset.0,
            scene.board_offset.1,
            scene.square_size,
            scene.pieces,
            scene.dragged,
        );
        self.text_pipeline.prepare(&self.queue, &all_texts);

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
            self.piece_pipeline.draw(&mut pass);
            self.overlay_pipeline.draw(&mut pass);
            self.text_pipeline.draw(&mut pass);
        }
        self.queue.submit(std::iter::once(encoder.finish()));
        output.present();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic stand-in for the glyph advance table: every character is
    /// 0.6 em wide.
    fn monospace(text: &str, size: f32) -> f32 {
        text.chars().count() as f32 * size * 0.6
    }

    #[test]
    fn short_banner_stays_one_line_at_full_size() {
        let (lines, size) = fit_banner("You win!", 500.0, 48.0, monospace);
        assert_eq!(lines, vec!["You win!"]);
        assert_eq!(size, 48.0);
    }

    #[test]
    fn long_banner_wraps_without_overflow() {
        let text = "Lichess rejected the token - check your API token";
        let (lines, size) = fit_banner(text, 400.0, 48.0, monospace);
        assert!(lines.len() > 1);
        for line in &lines {
            assert!(
                monospace(line, size) <= 400.0,
                "{line:?} overflows at {size}"
            );
        }
        assert_eq!(lines.join(" "), text);
    }

    #[test]
    fn banner_shrinks_then_ellipsizes_to_four_lines() {
        let text = vec!["x"; 400].join(" ");
        let (lines, size) = fit_banner(&text, 400.0, 64.0, monospace);
        assert!(size < 64.0);
        assert_eq!(lines.len(), BANNER_MAX_LINES);
        assert!(lines.last().unwrap().ends_with("..."));
        for line in &lines {
            assert!(
                monospace(line, size) <= 400.0,
                "{line:?} overflows at {size}"
            );
        }
    }

    #[test]
    fn oversized_word_is_hard_broken() {
        let text = "a".repeat(100);
        let (lines, size) = fit_banner(&text, 120.0, 20.0, monospace);
        for line in &lines {
            assert!(
                monospace(line, size) <= 120.0,
                "{line:?} overflows at {size}"
            );
        }
    }
}

