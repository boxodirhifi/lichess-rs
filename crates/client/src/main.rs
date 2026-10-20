use winit::application::ApplicationHandler;
use winit::event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy};
use winit::window::{Window, WindowId};
use winit::event::{WindowEvent, MouseButton, ElementState, KeyEvent};
use winit::keyboard::{KeyCode, PhysicalKey};
use std::sync::Arc;
use std::time::Instant;

#[derive(Debug, Clone)]
pub enum NetEvent {
    FeaturedGame {
        fen: String,
        white_name: String,
        white_rating: u32,
        black_name: String,
        black_rating: u32,
        white_secs: u32,
        black_secs: u32,
    },
    FenUpdate {
        fen: String,
        white_secs: u32,
        black_secs: u32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Local,
    Live,
}

struct LiveGameInfo {
    white_name: String,
    black_name: String,
    white_rating: u32,
    black_rating: u32,
    white_clock: f64,
    black_clock: f64,
    side_to_move: chess::Color,
    last_update: Instant,
    game_over: bool,
}

struct App {
    renderer: Option<renderer::Renderer>,
    window: Option<Arc<Window>>,
    ui: ui::UI,
    board: chess::Board,
    live_board: Option<chess::Board>,
    live_info: Option<LiveGameInfo>,
    side: chess::Color,
    status: chess::GameStatus,
    mode: Mode,
    cursor_pos: (f64, f64),
}

fn side_from_fen(fen: &str) -> chess::Color {
    match fen.split_whitespace().nth(1) {
        Some("b") => chess::Color::Black,
        _ => chess::Color::White,
    }
}

fn format_clock(secs: f64) -> String {
    let secs = secs.max(0.0) as u64;
    format!("{:02}:{:02}", secs / 60, secs % 60)
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

    fn build_texts(&self) -> Vec<renderer::TextElement> {
        let mut texts = Vec::new();
        if self.mode != Mode::Live {
            return texts;
        }
        let info = match &self.live_info {
            Some(i) => i,
            None => return texts,
        };

        let sq = self.ui.square_size();
        let board_px = sq * 8.0;
        let (ox, oy) = self.ui.board_offset();
        let left = ox;
        let right = ox + board_px;
        let font_size = (sq * 0.35).max(14.0);
        let white = [1.0, 1.0, 1.0, 1.0];

        // Freeze the clocks once the game is over so they don't drain forever.
        let elapsed = if info.game_over {
            0.0
        } else {
            info.last_update.elapsed().as_secs_f64()
        };
        let white_clock = if info.side_to_move == chess::Color::White {
            (info.white_clock - elapsed).max(0.0)
        } else {
            info.white_clock
        };
        let black_clock = if info.side_to_move == chess::Color::Black {
            (info.black_clock - elapsed).max(0.0)
        } else {
            info.black_clock
        };

        // Black bar (above the board)
        let top_y = oy - font_size * 0.4;
        texts.push(renderer::TextElement {
            text: format!("{} ({})", info.black_name, info.black_rating),
            x: left, y: top_y, size: font_size, color: white,
            align: renderer::TextAlign::Left,
        });
        texts.push(renderer::TextElement {
            text: format_clock(black_clock),
            x: right, y: top_y, size: font_size, color: white,
            align: renderer::TextAlign::Right,
        });

        // White bar (below the board)
        let bottom_y = oy + board_px + font_size * 1.1;
        texts.push(renderer::TextElement {
            text: format!("{} ({})", info.white_name, info.white_rating),
            x: left, y: bottom_y, size: font_size, color: white,
            align: renderer::TextAlign::Left,
        });
        texts.push(renderer::TextElement {
            text: format_clock(white_clock),
            x: right, y: bottom_y, size: font_size, color: white,
            align: renderer::TextAlign::Right,
        });

        texts
    }

    /// Play a completed promotion and return to normal play.
    fn finish_promotion(&mut self, kind: chess::PieceKind) {
        let Some((from, to, _)) = self.ui.close_promotion() else {
            return;
        };
        if let Some(mv) = self
            .board
            .legal_moves_from(from)
            .into_iter()
            .find(|m| m.to == to && m.promotion == Some(kind))
        {
            self.board.make_move(mv);
            self.side = self.side.opposite();
            self.status = self.board.status(self.side);
            self.refresh_check_highlight();
        }
        self.ui.clear_selection();
    }

    fn handle_board_click(&mut self, x: f32, y: f32) {
        if self.mode == Mode::Live {
            return;
        }
        if self.status != chess::GameStatus::Ongoing {
            return;
        }
        // A click while the promotion menu is open either picks a piece or
        // dismisses the menu and keeps the pawn selected.
        if self.ui.promotion_open() {
            match self.ui.promotion_at(x, y) {
                Some(kind) => self.finish_promotion(kind),
                None => {
                    self.ui.close_promotion();
                }
            }
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
                let candidates: Vec<chess::Move> = self
                    .board
                    .legal_moves_from(from)
                    .into_iter()
                    .filter(|m| m.to == sq)
                    .collect();
                let promotion = candidates.iter().any(|m| m.promotion.is_some());
                if promotion {
                    // Let the player pick the piece before playing the move.
                    self.ui.open_promotion(from, sq, self.side);
                    return;
                }
                if let Some(&mv) = candidates.first() {
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

        // Request the very first frame
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: NetEvent) {
        match event {
            NetEvent::FeaturedGame { fen, white_name, white_rating, black_name, black_rating, white_secs, black_secs } => {
                if let Some(board) = chess::Board::from_fen(&fen) {
                    self.live_board = Some(board);
                }
                self.live_info = Some(LiveGameInfo {
                    white_name,
                    black_name,
                    white_rating,
                    black_rating,
                    white_clock: white_secs as f64,
                    black_clock: black_secs as f64,
                    side_to_move: side_from_fen(&fen),
                    last_update: Instant::now(),
                    game_over: false,
                });
            }
            NetEvent::FenUpdate { fen, white_secs, black_secs } => {
                if let Some(board) = chess::Board::from_fen(&fen) {
                    let side = side_from_fen(&fen);
                    let terminal = !matches!(board.status(side), chess::GameStatus::Ongoing);
                    self.live_board = Some(board);
                    if let Some(info) = &mut self.live_info {
                        info.white_clock = white_secs as f64;
                        info.black_clock = black_secs as f64;
                        info.side_to_move = side;
                        info.last_update = Instant::now();
                        if terminal || white_secs == 0 || black_secs == 0 {
                            info.game_over = true;
                        }
                    }
                }
            }
        }

        // A network event updated the board or clocks, so we need a new frame
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let mut needs_redraw = false;

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
                needs_redraw = true;
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor_pos = (position.x, position.y);
                // Only redraw on mouse move if we need to update the promotion hover highlight
                if self.ui.promotion_open() {
                    needs_redraw = true;
                }
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                let (x, y) = self.cursor_pos;
                self.handle_board_click(x as f32, y as f32);
                needs_redraw = true;
            }
            WindowEvent::KeyboardInput {
                event: KeyEvent {
                    physical_key: PhysicalKey::Code(code),
                    state: ElementState::Pressed,
                    ..
                },
                ..
            } => {
                match code {
                    KeyCode::KeyL => {
                        self.mode = Mode::Live;
                        self.ui.clear_selection();
                        self.ui.clear_promotion();
                        self.ui.set_checked_king(None);
                        needs_redraw = true;
                    }
                    KeyCode::KeyP => {
                        self.mode = Mode::Local;
                        self.refresh_check_highlight();
                        needs_redraw = true;
                    }
                    KeyCode::Escape => {
                        self.ui.close_promotion();
                        needs_redraw = true;
                    }
                    _ => {}
                }
            }
            WindowEvent::RedrawRequested => {
                let texts = self.build_texts();
                let cursor = Some((self.cursor_pos.0 as f32, self.cursor_pos.1 as f32));
                let mut quads: Vec<renderer::Quad> = self.ui.squares.iter().map(|r| {
                    renderer::Quad { x: r.x, y: r.y, w: r.w, h: r.h, color: r.color }
                }).collect();

                for r in self.ui.promotion_quads(cursor) {
                    quads.push(renderer::Quad { x: r.x, y: r.y, w: r.w, h: r.h, color: r.color });
                }
                let pieces: Vec<renderer::PieceElement> = self.ui.promotion_choices().iter().map(|c| {
                    renderer::PieceElement {
                        kind: c.kind,
                        color: c.color,
                        x: c.rect.x + c.rect.w * 0.5,
                        y: c.rect.y + c.rect.h * 0.5,
                        size: c.rect.h,
                    }
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

                if let Some(rend) = self.renderer.as_mut() {
                    let scene = renderer::Scene {
                        quads: &quads,
                        board: match self.mode {
                            Mode::Live => self.live_board.as_ref().unwrap_or(&self.board),
                            Mode::Local => &self.board,
                        },
                        board_offset: self.ui.board_offset(),
                        square_size: self.ui.square_size(),
                        banner,
                        texts: &texts,
                        pieces: &pieces,
                    };
                    rend.render(&scene);
                }
            }
            _ => {}
        }

        // ONLY request a redraw if visual state actually changed
        if needs_redraw {
            if let Some(window) = self.window.as_ref() {
                window.request_redraw();
            }
        }
    }
}

fn lichess_worker(proxy: EventLoopProxy<NetEvent>) {
    loop {
        log::info!("Connecting to Lichess TV stream...");
        let result = lichess::stream_tv_feed(|event| {
            match event {
                lichess::TvStreamEvent::Featured(data) => {
                    let mut white_name = String::new();
                    let mut black_name = String::new();
                    let (mut white_rating, mut black_rating) = (0, 0);
                    let (mut white_secs, mut black_secs) = (0, 0);
                    for p in &data.players {
                        if p.color == "white" {
                            white_name = p.user.name.clone();
                            white_rating = p.rating;
                            white_secs = p.seconds;
                        } else {
                            black_name = p.user.name.clone();
                            black_rating = p.rating;
                            black_secs = p.seconds;
                        }
                    }
                    let _ = proxy.send_event(NetEvent::FeaturedGame {
                        fen: data.fen,
                        white_name, white_rating,
                        black_name, black_rating,
                        white_secs, black_secs,
                    });
                }
                lichess::TvStreamEvent::Fen(data) => {
                    let _ = proxy.send_event(NetEvent::FenUpdate {
                        fen: data.fen,
                        white_secs: data.wc,
                        black_secs: data.bc,
                    });
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
        live_info: None,
        side: chess::Color::White,
        status: chess::GameStatus::Ongoing,
        mode: Mode::Live,
        cursor_pos: (0.0, 0.0),
    };
    event_loop.run_app(&mut app).unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A board with a white pawn on the 7th rank, ready to promote.
    fn app_with_promoting_pawn() -> App {
        let mut app = App {
            renderer: None,
            window: None,
            ui: ui::UI::new(),
            board: chess::Board::from_fen("8/4P3/8/8/8/8/8/4K2k w - - 0 1").unwrap(),
            live_board: None,
            live_info: None,
            side: chess::Color::White,
            status: chess::GameStatus::Ongoing,
            mode: Mode::Local,
            cursor_pos: (0.0, 0.0),
        };
        // 800x600: the board is 480px, offset (160, 60), squares 60px.
        app.ui.layout(800.0, 600.0);
        app
    }

    /// Center of a square in window coordinates.
    fn square_center(app: &App, sq: chess::Square) -> (f32, f32) {
        let (ox, oy) = app.ui.board_offset();
        let s = app.ui.square_size();
        (
            ox + (sq.file() as f32 + 0.5) * s,
            oy + (7.0 - sq.rank() as f32 + 0.5) * s,
        )
    }

    /// Center of a promotion menu cell.
    fn choice_center(app: &App, kind: chess::PieceKind) -> (f32, f32) {
        let cell = app
            .ui
            .promotion_choices()
            .iter()
            .find(|c| c.kind == kind)
            .expect("promotion choice missing");
        (
            cell.rect.x + cell.rect.w * 0.5,
            cell.rect.y + cell.rect.h * 0.5,
        )
    }

    #[test]
    fn promoting_opens_the_menu_instead_of_moving() {
        let mut app = app_with_promoting_pawn();
        let e7 = chess::Square::from_file_rank(4, 6).unwrap();
        let e8 = chess::Square::from_file_rank(4, 7).unwrap();

        let (x, y) = square_center(&app, e7);
        app.handle_board_click(x, y);
        assert_eq!(app.ui.selected_square(), Some(e7));

        let (x, y) = square_center(&app, e8);
        app.handle_board_click(x, y);

        assert!(app.ui.promotion_open(), "menu should be open after the click");
        assert_eq!(app.ui.promotion_from(), Some(e7));
        // The pawn has not moved yet.
        assert!(matches!(
            app.board.piece_at(e7).map(|p| p.kind),
            Some(chess::PieceKind::Pawn)
        ));
        assert!(app.board.piece_at(e8).is_none());
        // The game is still White's move.
        assert_eq!(app.side, chess::Color::White);
    }

    #[test]
    fn picking_a_choice_plays_that_piece() {
        let mut app = app_with_promoting_pawn();
        let e7 = chess::Square::from_file_rank(4, 6).unwrap();
        let e8 = chess::Square::from_file_rank(4, 7).unwrap();

        let (x, y) = square_center(&app, e7);
        app.handle_board_click(x, y);
        let (x, y) = square_center(&app, e8);
        app.handle_board_click(x, y);
        assert!(app.ui.promotion_open());

        let (x, y) = choice_center(&app, chess::PieceKind::Knight);
        app.handle_board_click(x, y);

        assert!(!app.ui.promotion_open(), "menu should close after picking");
        assert!(app.board.piece_at(e7).is_none());
        assert_eq!(
            app.board.piece_at(e8).map(|p| (p.kind, p.color)),
            Some((chess::PieceKind::Knight, chess::Color::White))
        );
        assert_eq!(app.side, chess::Color::Black);
    }

    #[test]
    fn every_choice_produces_its_piece() {
        for kind in [
            chess::PieceKind::Queen,
            chess::PieceKind::Rook,
            chess::PieceKind::Bishop,
            chess::PieceKind::Knight,
        ] {
            let mut app = app_with_promoting_pawn();
            let e7 = chess::Square::from_file_rank(4, 6).unwrap();
            let e8 = chess::Square::from_file_rank(4, 7).unwrap();

            let (x, y) = square_center(&app, e7);
            app.handle_board_click(x, y);
            let (x, y) = square_center(&app, e8);
            app.handle_board_click(x, y);

            let (x, y) = choice_center(&app, kind);
            app.handle_board_click(x, y);

            assert_eq!(
                app.board.piece_at(e8).map(|p| p.kind),
                Some(kind),
                "wrong piece for {kind:?}"
            );
        }
    }

    #[test]
    fn clicking_away_from_the_menu_dismisses_it() {
        let mut app = app_with_promoting_pawn();
        let e7 = chess::Square::from_file_rank(4, 6).unwrap();
        let e8 = chess::Square::from_file_rank(4, 7).unwrap();

        let (x, y) = square_center(&app, e7);
        app.handle_board_click(x, y);
        let (x, y) = square_center(&app, e8);
        app.handle_board_click(x, y);
        assert!(app.ui.promotion_open());

        // A click on the far corner of the window is not a choice.
        app.handle_board_click(2.0, 2.0);

        assert!(!app.ui.promotion_open());
        // The pawn is still on the 7th rank and still selected, so the player
        // can pick another piece without re-selecting.
        assert!(matches!(
            app.board.piece_at(e7).map(|p| p.kind),
            Some(chess::PieceKind::Pawn)
        ));
        assert_eq!(app.ui.selected_square(), Some(e7));
    }

    #[test]
    fn closing_the_menu_keeps_the_pawn_selected() {
        let mut app = app_with_promoting_pawn();
        let e7 = chess::Square::from_file_rank(4, 6).unwrap();
        let e8 = chess::Square::from_file_rank(4, 7).unwrap();

        let (x, y) = square_center(&app, e7);
        app.handle_board_click(x, y);
        let (x, y) = square_center(&app, e8);
        app.handle_board_click(x, y);
        assert!(app.ui.promotion_open());

        // What Escape runs; the pawn stays put and selected.
        app.ui.close_promotion();
        assert!(!app.ui.promotion_open());
        assert!(matches!(
            app.board.piece_at(e7).map(|p| p.kind),
            Some(chess::PieceKind::Pawn)
        ));
        assert_eq!(app.ui.selected_square(), Some(e7));
    }

    #[test]
    fn ordinary_still_moves_immediately() {
        let mut app = app_with_promoting_pawn();
        let d2 = chess::Square::from_file_rank(3, 1).unwrap();
        let d4 = chess::Square::from_file_rank(3, 3).unwrap();

        // A pawn on d2 is not promoting, so a two-square push just happens.
        app.board.set_piece(d2, Some(chess::Piece::new(chess::PieceKind::Pawn, chess::Color::White)));
        let (x, y) = square_center(&app, d2);
        app.handle_board_click(x, y);
        let (x, y) = square_center(&app, d4);
        app.handle_board_click(x, y);

        assert!(!app.ui.promotion_open());
        assert!(app.board.piece_at(d4).is_some());
        assert_eq!(app.side, chess::Color::Black);
    }
}
