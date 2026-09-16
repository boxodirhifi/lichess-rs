use winit::application::ApplicationHandler;
use winit::event_loop::{EventLoop, ActiveEventLoop};
use winit::window::{Window, WindowId};
use winit::event::{WindowEvent, MouseButton, ElementState};
use std::sync::Arc;

struct App {
    renderer: Option<renderer::Renderer>,
    window: Option<Arc<Window>>,
    ui: ui::UI,
    board: chess::Board,
    side: chess::Color,
    cursor_pos: (f64, f64),
}

impl App {
    fn handle_board_click(&mut self, x: f32, y: f32) {
        let sq = match self.ui.square_at(x, y) {
            Some(sq) => sq,
            None => {
                self.ui.clear_selection();
                return;
            }
        };

        // Clicking a legal destination makes the move.
        if self.ui.is_move_target(sq) {
            if let Some(from) = self.ui.selected_square() {
                if let Some(mv) = self.board.moves_from(from).into_iter().find(|m| m.to == sq) {
                    self.board.make_move(mv);
                    self.side = self.side.opposite();
                }
            }
            self.ui.clear_selection();
            return;
        }

        // Otherwise select one of our own pieces.
        if let Some(piece) = self.board.piece_at(sq) {
            if piece.color == self.side {
                let mut targets: Vec<chess::Square> =
                    self.board.moves_from(sq).iter().map(|m| m.to).collect();
                targets.sort_by_key(|s| s.index());
                targets.dedup();
                self.ui.select(sq, targets);
                return;
            }
        }
        self.ui.clear_selection();
    }
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

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                self.renderer = None;
                self.window = None;
                event_loop.exit();
            }
            WindowEvent::Resized(size) => {
                if let Some(rend) = self.renderer.as_mut() {
                    rend.resize(size);
                    self.ui.layout(size.width.max(1) as f32, size.height.max(1) as f32);
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor_pos = (position.x, position.y);
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                let (x, y) = self.cursor_pos;
                self.handle_board_click(x as f32, y as f32);
            }
            WindowEvent::RedrawRequested => {
                if let Some(rend) = self.renderer.as_mut() {
                    let quads: Vec<renderer::Quad> = self.ui.squares.iter().map(|r| {
                        renderer::Quad { x: r.x, y: r.y, w: r.w, h: r.h, color: r.color }
                    }).collect();
                    let scene = renderer::Scene {
                        quads: &quads,
                        board: &self.board,
                        board_offset: self.ui.board_offset(),
                        square_size: self.ui.square_size(),
                    };
                    rend.render(&scene);
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
        board: chess::Board::starting_position(),
        side: chess::Color::White,
        cursor_pos: (0.0, 0.0),
    };
    event_loop.run_app(&mut app).unwrap();
}
