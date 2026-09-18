use winit::application::ApplicationHandler;
use winit::event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy};
use winit::window::{Window, WindowId};
use winit::event::{WindowEvent, MouseButton, ElementState, KeyEvent};
use winit::keyboard::{KeyCode, PhysicalKey};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub enum NetEvent {
    FeaturedGame { game_id: String, fen: String },
    FenUpdate { fen: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Local,
    Live,
}

struct App {
    renderer: Option<renderer::Renderer>,
    window: Option<Arc<Window>>,
    ui: ui::UI,
    board: chess::Board,
    live_board: Option<chess::Board>,
    side: chess::Color,
    status: chess::GameStatus,
    mode: Mode,
    cursor_pos: (f64, f64),
}

impl App {
    fn refresh_check_highlight(&mut self) {
        let checked = if self.board.in_check(self.side) {
            self.board.king_square(self.side)
        } else {
            None
        };
        self.ui.set_checked_king(checked);
    }

    fn handle_board_click(&mut self, x: f32, y: f32) {
        if self.mode == Mode::Live {
            return;
        }
        if self.status != chess::GameStatus::Ongoing {
            return;
        }

        let sq = match self.ui.square_at(x, y) {
            Some(sq) => sq,
            None => {
                self.ui.clear_selection();
                return;
            }
        };

        if self.ui.is_move_target(sq) {
            if let Some(from) = self.ui.selected_square() {
                if let Some(mv) = self.board.legal_moves_from(from).into_iter().find(|m| m.to == sq) {
                    self.board.make_move(mv);
                    self.side = self.side.opposite();
                    self.status = self.board.status(self.side);
                    self.refresh_check_highlight();
                }
            }
            self.ui.clear_selection();
            return;
        }

        if let Some(piece) = self.board.piece_at(sq) {
            if piece.color == self.side {
                let mut targets: Vec<chess::Square> =
                    self.board.legal_moves_from(sq).iter().map(|m| m.to).collect();
                targets.sort_by_key(|s| s.index());
                targets.dedup();
                self.ui.select(sq, targets);
                return;
            }
        }
        self.ui.clear_selection();
    }
}

impl ApplicationHandler<NetEvent> for App {
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

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: NetEvent) {
        match event {
            NetEvent::FeaturedGame { game_id: _, fen } => {
                if let Some(board) = chess::Board::from_fen(&fen) {
                    self.live_board = Some(board);
                }
            }
            NetEvent::FenUpdate { fen } => {
                if let Some(board) = chess::Board::from_fen(&fen) {
                    self.live_board = Some(board);
                }
            }
        }
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
            WindowEvent::KeyboardInput {
                event: KeyEvent {
                    physical_key: PhysicalKey::Code(code),
                    state: ElementState::Pressed,
                    ..
                },
                ..
            } => match code {
                KeyCode::KeyL => {
                    self.mode = Mode::Live;
                    self.ui.clear_selection();
                    self.ui.set_checked_king(None);
                }
                KeyCode::KeyP => {
                    self.mode = Mode::Local;
                    self.refresh_check_highlight();
                }
                _ => {}
            },
            WindowEvent::RedrawRequested => {
                if let Some(rend) = self.renderer.as_mut() {
                    let quads: Vec<renderer::Quad> = self.ui.squares.iter().map(|r| {
                        renderer::Quad { x: r.x, y: r.y, w: r.w, h: r.h, color: r.color }
                    }).collect();

                    let banner: Option<&'static str> = if self.mode == Mode::Local {
                        match self.status {
                            chess::GameStatus::Checkmate(chess::Color::White) => Some("Checkmate - White wins"),
                            chess::GameStatus::Checkmate(chess::Color::Black) => Some("Checkmate - Black wins"),
                            chess::GameStatus::Stalemate => Some("Stalemate - Draw"),
                            chess::GameStatus::Ongoing => None,
                        }
                    } else {
                        None
                    };

                    let scene = renderer::Scene {
                        quads: &quads,
                        board: match self.mode {
                            Mode::Live => self.live_board.as_ref().unwrap_or(&self.board),
                            Mode::Local => &self.board,
                        },
                        board_offset: self.ui.board_offset(),
                        square_size: self.ui.square_size(),
                        banner,
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

fn lichess_worker(proxy: EventLoopProxy<NetEvent>) {
    loop {
        log::info!("Connecting to Lichess TV stream...");
        let result = lichess::stream_tv_feed(|event| {
            match event {
                lichess::TvStreamEvent::Featured(data) => {
                    let _ = proxy.send_event(NetEvent::FeaturedGame {
                        game_id: data.id,
                        fen: data.fen,
                    });
                }
                lichess::TvStreamEvent::Fen(data) => {
                    let _ = proxy.send_event(NetEvent::FenUpdate { fen: data.fen });
                }
            }
        });

        if let Err(e) = result {
            log::warn!("Stream ended: {}, reconnecting in 1s", e);
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    }
}

fn main() {
    env_logger::init();
    let event_loop = EventLoop::<NetEvent>::with_user_event().build().unwrap();
    let proxy = event_loop.create_proxy();
    std::thread::spawn(move || lichess_worker(proxy));

    let mut app = App {
        renderer: None,
        window: None,
        ui: ui::UI::new(),
        board: chess::Board::starting_position(),
        live_board: None,
        side: chess::Color::White,
        status: chess::GameStatus::Ongoing,
        mode: Mode::Live,
        cursor_pos: (0.0, 0.0),
    };
    event_loop.run_app(&mut app).unwrap();
}
