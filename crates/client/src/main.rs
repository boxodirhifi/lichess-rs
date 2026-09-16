use winit::application::ApplicationHandler;
use winit::event_loop::{EventLoop, ActiveEventLoop};
use winit::window::{Window, WindowId};
use winit::event::WindowEvent;
use std::sync::Arc;

struct App {
    // IMPORTANT: renderer MUST be declared before window so it drops first.
    // The GPU surface needs the window alive during cleanup.
    renderer: Option<renderer::Renderer>,
    window: Option<Arc<Window>>,
    ui: ui::UI,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let attrs = platform::create_window_attrs();
        let window = Arc::new(event_loop.create_window(attrs).unwrap());
        let size = window.inner_size();

        let safe_w = size.width.max(1) as f32;
        let safe_h = size.height.max(1) as f32;

        self.ui.layout(safe_w, safe_h);
        let rend = pollster::block_on(renderer::Renderer::new(window.clone()));

        self.window = Some(window);
        self.renderer = Some(rend);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => {
                // Explicitly drop GPU resources before the window and event loop
                self.renderer = None;
                self.window = None;
                event_loop.exit();
            }
            WindowEvent::Resized(size) => {
                if let Some(rend) = self.renderer.as_mut() {
                    rend.resize(size);
                    let safe_w = size.width.max(1) as f32;
                    let safe_h = size.height.max(1) as f32;
                    self.ui.layout(safe_w, safe_h);
                }
            }
            WindowEvent::RedrawRequested => {
                if let Some(rend) = self.renderer.as_mut() {
                    let quads: Vec<renderer::Quad> = self.ui.squares.iter().map(|r| {
                        renderer::Quad {
                            x: r.x, y: r.y, w: r.w, h: r.h, color: r.color,
                        }
                    }).collect();
                    rend.render(&quads);
                }
            }
            _ => {}
        }

        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }
}

fn main() {
    env_logger::init();
    let event_loop = EventLoop::new().unwrap();
    let mut app = App {
        renderer: None,
        window: None,
        ui: ui::UI::new(),
    };
    event_loop.run_app(&mut app).unwrap();
}
