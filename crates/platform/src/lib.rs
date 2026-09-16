use winit::window::{Window, WindowAttributes};

pub fn create_window_attrs() -> WindowAttributes {
    Window::default_attributes()
        .with_title("Lichess-rs")
        .with_inner_size(winit::dpi::LogicalSize::new(800, 600))
}
