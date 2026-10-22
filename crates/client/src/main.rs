use winit::application::ApplicationHandler;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::window::{Window, WindowId};
use winit::event::{WindowEvent, MouseButton, ElementState, KeyEvent};
use winit::keyboard::{KeyCode, PhysicalKey};
use std::sync::Arc;
use std::time::{Duration, Instant};

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
    /// Token validated; carries the account name.
    OnlineReady {
        username: String,
    },
    /// A Board API game (accepted challenge) is set up.
    OnlineFull {
        game_id: String,
        initial_fen: Option<String>,
        white_name: String,
        white_rating: u32,
        black_name: String,
        black_rating: u32,
        moves: String,
        wtime_ms: u64,
        btime_ms: u64,
        status: String,
        winner: Option<chess::Color>,
    },
    /// Incremental Board API state update.
    OnlineState {
        moves: String,
        wtime_ms: u64,
        btime_ms: u64,
        status: String,
        winner: Option<chess::Color>,
    },
    OnlineFailed {
        message: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Local,
    Live,
    /// Online play through the Board API (challenge a friend and play).
    Play,
}

/// An online game in progress.
struct OnlineGame {
    game_id: String,
    board: chess::Board,
    my_color: chess::Color,
    white_name: String,
    black_name: String,
    white_rating: u32,
    black_rating: u32,
    white_clock: f64,
    black_clock: f64,
    last_update: Instant,
    game_over: bool,
    winner: Option<chess::Color>,
}

enum Online {
    Idle,
    /// Challenge created, waiting for the opponent's acceptance.
    Waiting,
    /// Boxed: the running game is much larger than the marker variants.
    Active(Box<OnlineGame>),
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
    /// Event proxy for spawning online-play workers; `None` in unit tests.
    proxy: Option<EventLoopProxy<NetEvent>>,
    renderer: Option<renderer::Renderer>,
    window: Option<Arc<Window>>,
    ui: ui::UI,
    board: chess::Board,
    live_board: Option<chess::Board>,
    live_info: Option<LiveGameInfo>,
    mode: Mode,
    cursor_pos: (f64, f64),
    side: chess::Color,
    status: chess::GameStatus,
    token: Option<String>,
    my_username: Option<String>,
    online: Online,
    online_error: Option<String>,
}

fn side_from_fen(fen: &str) -> chess::Color {
    match fen.split_whitespace().nth(1) {
        Some("b") => chess::Color::Black,
        _ => chess::Color::White,
    }
}

// Stage 1 bridge: rendering speaks shakmaty, the rest of the app still uses
// the local `chess` crate. These helpers disappear in Stage 2.
fn to_shakmaty_color(color: chess::Color) -> shakmaty::Color {
    match color {
        chess::Color::White => shakmaty::Color::White,
        chess::Color::Black => shakmaty::Color::Black,
    }
}

fn to_shakmaty_role(kind: chess::PieceKind) -> shakmaty::Role {
    match kind {
        chess::PieceKind::Pawn => shakmaty::Role::Pawn,
        chess::PieceKind::Knight => shakmaty::Role::Knight,
        chess::PieceKind::Bishop => shakmaty::Role::Bishop,
        chess::PieceKind::Rook => shakmaty::Role::Rook,
        chess::PieceKind::Queen => shakmaty::Role::Queen,
        chess::PieceKind::King => shakmaty::Role::King,
    }
}

/// Rebuild the position as a shakmaty `VariantPosition` from the board's FEN.
/// Stage 1 only handles standard chess; variant setup arrives in Stage 3.
fn to_variant_position(board: &chess::Board) -> shakmaty::variant::VariantPosition {
    let fen = board.to_fen();
    shakmaty::fen::Fen::from_ascii(fen.as_bytes())
        .ok()
        .and_then(|fen| {
            fen.into_position::<shakmaty::Chess>(shakmaty::CastlingMode::Standard)
                .ok()
                .map(shakmaty::variant::VariantPosition::from)
        })
        .unwrap_or_default()
}

fn format_clock(secs: f64) -> String {
    let secs = secs.max(0.0) as u64;
    format!("{:02}:{:02}", secs / 60, secs % 60)
}

/// Coordinates are tinted with the opposite colour of the square they sit on,
/// so they always contrast — the same trick Lichess and Chess.com use.
const COORD_ON_LIGHT: [f32; 4] = [181.0 / 255.0, 136.0 / 255.0, 99.0 / 255.0, 1.0];
const COORD_ON_DARK: [f32; 4] = [240.0 / 255.0, 217.0 / 255.0, 181.0 / 255.0, 1.0];

fn coord_color(file: u8, rank: u8) -> [f32; 4] {
    // a1 is dark; a square is light when file + rank is odd.
    if (file as u32 + rank as u32) % 2 == 1 {
        COORD_ON_LIGHT
    } else {
        COORD_ON_DARK
    }
}

impl App {
    /// Kick off the online flow: read the token, read the opponent, and hand
    /// the long-running work (challenge creation + game stream) to a worker
    /// thread. Guards against double-starting while one is running.
    fn start_online_challenge(&mut self) {
        if !matches!(self.online, Online::Idle) {
            return;
        }
        let Some(token) = lichess::read_token() else {
            self.online_error = Some(
                "No API token — set LICHESS_API_TOKEN or ~/.config/lichess-rs/token".to_string(),
            );
            return;
        };
        let Some(opponent) = read_opponent() else {
            self.online_error = Some(
                "No opponent — set LICHESS_OPPONENT or ~/.config/lichess-rs/opponent".to_string(),
            );
            return;
        };
        self.token = Some(token.clone());
        self.online = Online::Waiting;
        let Some(proxy) = self.proxy.clone() else {
            return;
        };
        std::thread::spawn(move || online_worker(proxy, token, opponent));
    }

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
        self.push_coordinates(&mut texts);
        if self.mode == Mode::Live {
            self.push_player_info(&mut texts);
        }
        if self.mode == Mode::Play {
            self.push_online_info(&mut texts);
        }
        texts
    }

    /// Player bars for the online game, black above the board and white
    /// below it, in the same style as the spectator bars.
    fn push_online_info(&self, texts: &mut Vec<renderer::TextElement>) {
        let game = match &self.online {
            Online::Active(game) => game,
            _ => return,
        };

        let sq = self.ui.square_size();
        let board_px = sq * 8.0;
        let (ox, oy) = self.ui.board_offset();
        let left = ox;
        let right = ox + board_px;
        let font_size = (sq * 0.35).max(14.0);
        let white = [1.0, 1.0, 1.0, 1.0];

        // Same server-truth freeze rule as spectating.
        let elapsed = if game.game_over {
            0.0
        } else {
            game.last_update.elapsed().as_secs_f64()
        };
        let white_clock = if game.board.side_to_move() == chess::Color::White {
            (game.white_clock - elapsed).max(0.0)
        } else {
            game.white_clock
        };
        let black_clock = if game.board.side_to_move() == chess::Color::Black {
            (game.black_clock - elapsed).max(0.0)
        } else {
            game.black_clock
        };

        let mine = |name: &str| -> bool {
            self.my_username
                .as_deref()
                .map(|user| user.eq_ignore_ascii_case(name))
                .unwrap_or(false)
        };
        let label = |name: &str, rating: u32| -> String {
            let star = if mine(name) { " *" } else { "" };
            format!("{name} ({rating}){star}")
        };

        // Black's bar sits above the board, white's below it.
        let bars = [
            (
                game.black_name.clone(),
                game.black_rating,
                black_clock,
                left,
                oy - font_size * 0.4,
            ),
            (
                game.white_name.clone(),
                game.white_rating,
                white_clock,
                left,
                oy + board_px + font_size * 1.1,
            ),
        ];
        for (name, rating, clock, label_x, y) in bars {
            texts.push(renderer::TextElement {
                text: label(&name, rating),
                x: label_x,
                y,
                size: font_size,
                color: white,
                align: renderer::TextAlign::Left,
                shadow: false,
            });
            texts.push(renderer::TextElement {
                text: format_clock(clock),
                x: right,
                y,
                size: font_size,
                color: white,
                align: renderer::TextAlign::Right,
                shadow: false,
            });
        }
    }

    /// The clock display subtracts elapsed time at redraw time — so unless a
    /// redraw is scheduled, clocks would appear frozen between board/network
    /// updates (e.g. all through a 10+0 thinking streak). Returns the next
    /// whole-second boundary on which the tick bar should be redrawn, in both
    /// spectator and online-play mode; `None` when nothing can tick.
    fn next_clock_tick(&self) -> Option<Instant> {
        let candidates = [
            match &self.live_info {
                Some(info) if self.mode == Mode::Live && !info.game_over => Some(&info.last_update),
                _ => None,
            },
            match &self.online {
                Online::Active(game) if self.mode == Mode::Play && !game.game_over => {
                    Some(&game.last_update)
                }
                _ => None,
            },
        ];
        candidates
            .into_iter()
            .flatten()
            .map(|last_update| {
                *last_update + Duration::from_secs(last_update.elapsed().as_secs() + 1)
            })
            .min()
    }

    /// Lichess-style in-square coordinates: file letters in the bottom-left
    /// corner of the first-rank squares, rank numbers in the top-right corner
    /// of the h-file squares.
    fn push_coordinates(&self, texts: &mut Vec<renderer::TextElement>) {
        let sq = self.ui.square_size();
        if sq <= 0.0 {
            return;
        }
        let (ox, oy) = self.ui.board_offset();
        let size = sq * 0.22;
        let pad = sq * 0.07;

        // Files a-h along the bottom rank (screen row 7).
        for file in 0..8u8 {
            let sq_x = ox + file as f32 * sq;
            let sq_y = oy + 7.0 * sq;
            texts.push(renderer::TextElement {
                text: ((b'a' + file) as char).to_string(),
                x: sq_x + pad,
                y: sq_y + sq - pad,
                size,
                color: coord_color(file, 0),
                align: renderer::TextAlign::Left,
                shadow: false,
            });
        }

        // Ranks 1-8 along the h-file (screen column 7).
        for rank in 0..8u8 {
            let sq_x = ox + 7.0 * sq;
            let sq_y = oy + (7.0 - rank as f32) * sq;
            texts.push(renderer::TextElement {
                text: ((b'1' + rank) as char).to_string(),
                x: sq_x + sq - pad,
                y: sq_y + pad + size,
                size,
                color: coord_color(7, rank),
                align: renderer::TextAlign::Right,
                shadow: false,
            });
        }
    }

    fn push_player_info(&self, texts: &mut Vec<renderer::TextElement>) {
        let info = match &self.live_info {
            Some(i) => i,
            None => return,
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
            shadow: false,
        });
        texts.push(renderer::TextElement {
            text: format_clock(black_clock),
            x: right, y: top_y, size: font_size, color: white,
            align: renderer::TextAlign::Right,
            shadow: false,
        });

        // White bar (below the board)
        let bottom_y = oy + board_px + font_size * 1.1;
        texts.push(renderer::TextElement {
            text: format!("{} ({})", info.white_name, info.white_rating),
            x: left, y: bottom_y, size: font_size, color: white,
            align: renderer::TextAlign::Left,
            shadow: false,
        });
        texts.push(renderer::TextElement {
            text: format_clock(white_clock),
            x: right, y: bottom_y, size: font_size, color: white,
            align: renderer::TextAlign::Right,
            shadow: false,
        });
    }

    /// Play a completed promotion and return to normal play.
    /// Which board clicks operate on: the local one in local play, the
    /// online one while a Board API game is running.
    fn active_board(&mut self) -> Option<&mut chess::Board> {
        match self.mode {
            Mode::Local => Some(&mut self.board),
            Mode::Play => match &mut self.online {
                Online::Active(game) => Some(&mut game.board),
                _ => None,
            },
            Mode::Live => None,
        }
    }

    /// Post a played move to Lichess in the background; the board stream
    /// echoes it back, so the reply only needs logging.
    fn post_online_move(&mut self, mv: chess::Move) {
        let game_id = self.online_game().map(|game| game.game_id.clone());
        let Some(game_id) = game_id else { return };
        let Some(token) = self.token.clone() else { return };
        let uci = mv.to_uci();
        std::thread::spawn(move || {
            if let Err(e) = lichess::play_board_move(&token, &game_id, &uci) {
                log::warn!("Move post failed: {}", e);
            }
        });
    }

    /// The running online game, if any.
    fn online_game(&self) -> Option<&OnlineGame> {
        match &self.online {
            Online::Active(game) => Some(game),
            _ => None,
        }
    }

    fn finish_promotion(&mut self, kind: chess::PieceKind) {
        let Some((from, to, _)) = self.ui.close_promotion() else {
            return;
        };
        let Some(mv) = self
            .active_board()
            .and_then(|board| {
                board
                    .legal_moves_from(from)
                    .into_iter()
                    .find(|m| m.to == to && m.promotion == Some(kind))
            })
        else {
            self.ui.clear_selection();
            return;
        };
        self.apply_move(mv);
        self.ui.clear_selection();
    }

    /// Apply a complete move to the active board: bookkeeping for local play,
    /// and a background POST for online play.
    fn apply_move(&mut self, mv: chess::Move) {
        if self.mode == Mode::Local {
            self.board.make_move(mv);
            self.side = self.side.opposite();
            self.status = self.board.status(self.side);
            self.refresh_check_highlight();
        } else {
            self.post_online_move(mv);
        }
    }

    fn handle_board_click(&mut self, x: f32, y: f32) {
        if self.mode == Mode::Live {
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

        // Whose move it is, and whether moves are accepted at all: local
        // play accepts everything until the game ends; online play only my
        // side, only while the server says the game is still running.
        let my_turn = match self.mode {
            Mode::Local => self.status == chess::GameStatus::Ongoing,
            Mode::Play => match self.online_game() {
                Some(game) => {
                    !game.game_over && game.board.side_to_move() == game.my_color
                }
                None => return,
            },
            Mode::Live => unreachable!(),
        };

        let sq = match self.ui.square_at(x, y) {
            Some(sq) => sq,
            None => {
                self.ui.clear_selection();
                return;
            }
        };
        if self.ui.is_move_target(sq) {
            if let Some(from) = self.ui.selected_square() {
                let candidates = self
                    .active_board()
                    .map(|board| {
                        board
                            .legal_moves_from(from)
                            .into_iter()
                            .filter(|m| m.to == sq)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let promotion = candidates.iter().any(|m| m.promotion.is_some());
                if promotion && my_turn {
                    // Let the player pick the piece before playing the move.
                    let mover_color = self
                        .active_board()
                        .and_then(|board| board.piece_at(from))
                        .map(|piece| piece.color);
                    if let Some(color) = mover_color {
                        self.ui.open_promotion(from, sq, color);
                    }
                    // Keep the pawn selected: picking a non-menu cell then
                    // returns to offering the remaining choices.
                    return;
                } else if let Some(&mv) = candidates.first() {
                    if my_turn {
                        self.apply_move(mv);
                    }
                }
            }
            self.ui.clear_selection();
            return;
        }
        if let Some(piece) = self.active_board().and_then(|board| board.piece_at(sq)) {
            if self.can_select_piece(piece.color) {
                let mut targets: Vec<chess::Square> = self
                    .active_board()
                    .map(|board| board.legal_moves_from(sq).iter().map(|m| m.to).collect())
                    .unwrap_or_default();
                targets.sort_by_key(|s| s.index());
                targets.dedup();
                self.ui.select(sq, targets);
                return;
            }
        }
        self.ui.clear_selection();
    }

    /// Selection rules: local play lets you pick either color; online play
    /// only your own pieces, while it is your turn.
    fn can_select_piece(&self, color: chess::Color) -> bool {
        match self.mode {
            Mode::Local => true,
            Mode::Play => self
                .online_game()
                .map(|game| game.my_color == color)
                .unwrap_or(false),
            Mode::Live => false,
        }
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
            NetEvent::OnlineReady { username } => {
                self.my_username = Some(username);
            }
            NetEvent::OnlineFull {
                game_id,
                initial_fen,
                white_name,
                black_name,
                white_rating,
                black_rating,
                moves,
                wtime_ms,
                btime_ms,
                status,
                winner,
            } => {
                let me = self.my_username.clone();
                let mine = |name: &str| me.as_deref().is_some_and(|me| me.eq_ignore_ascii_case(name));
                let my_color = if mine(&white_name) {
                    chess::Color::White
                } else {
                    chess::Color::Black
                };
                let board = match initial_fen.as_deref().unwrap_or("startpos") {
                    "startpos" => chess::Board::starting_position(),
                    fen => chess::Board::from_fen(fen).unwrap_or_else(chess::Board::starting_position),
                };
                let mut board = board;
                replay_uci(&mut board, &moves);
                self.online = Online::Active(Box::new(OnlineGame {
                    game_id,
                    board,
                    my_color,
                    white_name,
                    black_name,
                    white_rating,
                    black_rating,
                    white_clock: wtime_ms as f64 / 1000.0,
                    black_clock: btime_ms as f64 / 1000.0,
                    last_update: Instant::now(),
                    game_over: status != "started",
                    winner,
                }));
            }
            NetEvent::OnlineState { moves, wtime_ms, btime_ms, status, winner } => {
                if let Online::Active(game) = &mut self.online {
                    replay_uci(&mut game.board, &moves);
                    game.white_clock = wtime_ms as f64 / 1000.0;
                    game.black_clock = btime_ms as f64 / 1000.0;
                    game.last_update = Instant::now();
                    game.game_over = status != "started";
                    game.winner = winner;
                }
            }
            NetEvent::OnlineFailed { message } => {
                self.online_error = Some(message);
                self.online = Online::Idle;
            }
        }

        // A network event updated the board or clocks, so we need a new frame
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    /// Runs after every dispatched event. Here we keep the event-driven rule:
    /// when nothing needs a redraw we wait for the next real event, and the
    /// only timer we ever arm is the once-per-second clock tick while
    /// spectating a running game.
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(deadline) = self.next_clock_tick() {
            // The tick boundary has already passed: draw the clocks now, then
            // re-arm the timer for the following second.
            if Instant::now() >= deadline {
                if let Some(window) = self.window.as_ref() {
                    window.request_redraw();
                }
            }
            event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
        } else {
            event_loop.set_control_flow(ControlFlow::Wait);
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
                    KeyCode::KeyC => {
                        self.mode = Mode::Play;
                        self.ui.clear_selection();
                        self.ui.clear_promotion();
                        self.online_error = None;
                        self.start_online_challenge();
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
                        role: to_shakmaty_role(c.kind),
                        color: to_shakmaty_color(c.color),
                        x: c.rect.x + c.rect.w * 0.5,
                        y: c.rect.y + c.rect.h * 0.5,
                        size: c.rect.h,
                    }
                }).collect();
                let banner: Option<String> = match self.mode {
                    Mode::Local => match self.status {
                        chess::GameStatus::Checkmate(chess::Color::White) => Some("Checkmate - White wins"),
                        chess::GameStatus::Checkmate(chess::Color::Black) => Some("Checkmate - Black wins"),
                        chess::GameStatus::Stalemate => Some("Stalemate - Draw"),
                        chess::GameStatus::FiftyMoveRule => Some("Draw - 50-move rule"),
                        chess::GameStatus::ThreefoldRepetition => Some("Draw - threefold repetition"),
                        chess::GameStatus::InsufficientMaterial => Some("Draw - insufficient material"),
                        chess::GameStatus::Ongoing => None,
                    }
                    .map(|text| text.to_string()),
                    Mode::Live => None,
                    Mode::Play => {
                        if let Some(error) = &self.online_error {
                            Some(error.clone())
                        } else {
                            match &self.online {
                                Online::Waiting => {
                                    Some("Challenge sent - waiting for opponent to accept".to_string())
                                }
                                Online::Active(game) if game.game_over => {
                                    let text = match game.winner {
                                        Some(color) if color == game.my_color => "You win!",
                                        Some(_) => "You lose",
                                        None => "Drawn game",
                                    };
                                    Some(text.to_string())
                                }
                                _ => None,
                            }
                        }
                    }
                };
                let banner: Option<&str> = banner.as_deref();

                let board = match self.mode {
                    Mode::Live => self.live_board.as_ref().unwrap_or(&self.board),
                    Mode::Local => &self.board,
                    Mode::Play => match &self.online {
                        Online::Active(game) => &game.board,
                        _ => &self.board,
                    },
                };
                let board = to_variant_position(board);

                if let Some(rend) = self.renderer.as_mut() {
                    let scene = renderer::Scene {
                        quads: &quads,
                        board: &board,
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

/// Opponent name: the `LICHESS_OPPONENT` environment variable first, then
/// the first line of `~/.config/lichess-rs/opponent`.
fn read_opponent() -> Option<String> {
    if let Ok(name) = std::env::var("LICHESS_OPPONENT") {
        let name = name.trim().to_string();
        if !name.is_empty() {
            return Some(name);
        }
    }
    let config_home = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|dir| !dir.is_empty())
        .or_else(|| std::env::var("HOME").ok().map(|home| format!("{home}/.config")))?;
    let file = format!("{config_home}/lichess-rs/opponent");
    std::fs::read_to_string(file)
        .ok()
        .and_then(|contents| contents.lines().next().map(|line| line.trim().to_string()))
        .filter(|line| !line.is_empty())
}

/// Replay a full UCI move list (as the Board API stream sends it) onto a
/// board, using the board's own side-to-move tracking.
fn replay_uci(board: &mut chess::Board, moves: &str) {
    for uci in moves.split_whitespace() {
        let side = board.side_to_move();
        match board.move_from_uci(uci, side) {
            Some(mv) => board.make_move(mv),
            None => {
                log::warn!("UCI replay stopped at {}", uci);
                break;
            }
        }
    }
}

/// Long-running online play worker: validate the token, create the
/// challenge, wait for acceptance, then stream the game to the event loop
/// until it ends.
fn online_worker(proxy: EventLoopProxy<NetEvent>, token: String, opponent: String) {
    let me = match lichess::account_me(&token) {
        Ok(me) => me,
        Err(e) => {
            let _ = proxy.send_event(NetEvent::OnlineFailed {
                message: format!("Token check failed: {e}"),
            });
            return;
        }
    };
    let _ = proxy.send_event(NetEvent::OnlineReady { username: me.username });

    // Unrated 3+2 through the Board API.
    let challenge = match lichess::create_challenge(&token, &opponent, false, 180, 2, None) {
        Ok(challenge) => challenge,
        Err(e) => {
            let _ = proxy.send_event(NetEvent::OnlineFailed {
                message: format!("Could not challenge {opponent}: {e}"),
            });
            return;
        }
    };
    log::info!("Challenge {} sent to {}, streaming until accepted...", challenge.id, opponent);

    // The game stream 404s until the opponent accepts; retry for a bit, then
    // stream until (and only until) the game ends.
    for _ in 0..90 {
        let result = lichess::stream_board_game(&token, &challenge.id, |event| {
            forward_board_event(&proxy, event)
        });
        match result {
            Ok(()) => return, // stream closed = game over
            Err(e) if e.is_not_found() => {
                std::thread::sleep(std::time::Duration::from_secs(2));
            }
            Err(e) => {
                let _ = proxy.send_event(NetEvent::OnlineFailed {
                    message: format!("Game stream failed: {e}"),
                });
                return;
            }
        }
    }
    let _ = proxy.send_event(NetEvent::OnlineFailed {
        message: format!("{opponent} did not accept in time"),
    });
}

/// Map Board API stream events onto the app's event enum.
fn forward_board_event(proxy: &EventLoopProxy<NetEvent>, event: lichess::BoardStreamEvent) {
    let color_of = |name: &str| match name.to_ascii_lowercase().as_str() {
        "white" => Some(chess::Color::White),
        "black" => Some(chess::Color::Black),
        _ => None,
    };
    let send = match event {
        lichess::BoardStreamEvent::Full(full) => Some(NetEvent::OnlineFull {
            game_id: full.id,
            initial_fen: full.initial_fen,
            white_name: full.white.name,
            white_rating: full.white.rating.unwrap_or(0),
            black_name: full.black.name,
            black_rating: full.black.rating.unwrap_or(0),
            moves: full.state.moves,
            wtime_ms: full.state.wtime,
            btime_ms: full.state.btime,
            status: full.state.status,
            winner: full.state.winner.as_deref().and_then(color_of),
        }),
        lichess::BoardStreamEvent::State(state) => Some(NetEvent::OnlineState {
            moves: state.moves,
            wtime_ms: state.wtime,
            btime_ms: state.btime,
            status: state.status,
            winner: state.winner.as_deref().and_then(color_of),
        }),
        // Chat and opponent-gone notices are out of scope for now.
        lichess::BoardStreamEvent::Chat(_) | lichess::BoardStreamEvent::OpponentGone(_) => None,
    };
    if let Some(event) = send {
        let _ = proxy.send_event(event);
    }
}

fn main() {
    env_logger::init();
    let event_loop = EventLoop::<NetEvent>::with_user_event().build().unwrap();
    let proxy = event_loop.create_proxy();
    let app_proxy = proxy.clone();
    std::thread::spawn(move || lichess_worker(proxy));

    let mut app = App {
        proxy: Some(app_proxy),
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
        token: None,
        my_username: None,
        online: Online::Idle,
        online_error: None,
    };
    event_loop.run_app(&mut app).unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A board with a white pawn on the 7th rank, ready to promote.
    fn app_with_promoting_pawn() -> App {
        let mut app = App {
            proxy: None,
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
            token: None,
            my_username: None,
            online: Online::Idle,
            online_error: None,
        };
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
    fn replay_uci_applies_moves_with_side_to_move() {
        let mut board = chess::Board::starting_position();
        replay_uci(&mut board, "e2e4 g8f6 b1c3");
        assert_eq!(board.side_to_move(), chess::Color::Black);
        let c3 = chess::Square::from_file_rank(2, 2).unwrap();
        assert_eq!(
            board.piece_at(c3).map(|p| p.kind),
            Some(chess::PieceKind::Knight)
        );

        // Unknown moves stop the replay instead of panicking.
        let mut board = chess::Board::starting_position();
        replay_uci(&mut board, "e2e4 nonsense e7e5");
        assert_eq!(board.side_to_move(), chess::Color::Black);
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

    /// Text element whose contents match `needle`.
    fn text_named<'a>(texts: &'a [renderer::TextElement], needle: &str) -> &'a renderer::TextElement {
        texts
            .iter()
            .find(|t| t.text == needle)
            .unwrap_or_else(|| panic!("missing text {needle:?}"))
    }

    fn app_with_live_game() -> App {
        let mut app = app_with_promoting_pawn();
        app.mode = Mode::Live;
        app.live_board = Some(chess::Board::starting_position());
        app.live_info = Some(LiveGameInfo {
            white_name: "White".to_string(),
            black_name: "Black".to_string(),
            white_rating: 1500,
            black_rating: 1600,
            white_clock: 300.0,
            black_clock: 300.0,
            side_to_move: chess::Color::White,
            last_update: Instant::now(),
            game_over: false,
        });
        app
    }

    #[test]
    fn coordinates_render_in_both_modes() {
        let local = app_with_promoting_pawn();
        let live = app_with_live_game();

        for app in [&local, &live] {
            let texts = app.build_texts();
            for file in 0..8u8 {
                let label = ((b'a' + file) as char).to_string();
                assert!(texts.iter().any(|t| t.text == label), "missing file {label}");
            }
            for rank in 0..8u8 {
                let label = ((b'1' + rank) as char).to_string();
                assert!(texts.iter().any(|t| t.text == label), "missing rank {label}");
            }
        }
    }

    #[test]
    fn coordinates_sit_inside_the_edge_squares() {
        let app = app_with_live_game();
        let texts = app.build_texts();
        let (ox, oy) = app.ui.board_offset();
        let sq = app.ui.square_size();
        let size = sq * 0.22;
        let pad = sq * 0.07;

        // Files: bottom-left corner of the first-rank squares.
        for file in 0..8u8 {
            let label = ((b'a' + file) as char).to_string();
            let t = text_named(&texts, &label);
            let sq_x = ox + file as f32 * sq;
            let sq_y = oy + 7.0 * sq;
            assert!((t.x - (sq_x + pad)).abs() < 0.001, "{label} x off");
            assert!((t.y - (sq_y + sq - pad)).abs() < 0.001, "{label} y off");
            assert!(matches!(t.align, renderer::TextAlign::Left));
        }

        // Ranks: top-right corner of the h-file squares.
        for rank in 0..8u8 {
            let label = ((b'1' + rank) as char).to_string();
            let t = text_named(&texts, &label);
            let sq_x = ox + 7.0 * sq;
            let sq_y = oy + (7.0 - rank as f32) * sq;
            assert!((t.x - (sq_x + sq - pad)).abs() < 0.001, "{label} x off");
            assert!((t.y - (sq_y + pad + size)).abs() < 0.001, "{label} y off");
            assert!(matches!(t.align, renderer::TextAlign::Right));
        }
    }

    #[test]
    fn coordinates_contrast_with_their_square() {
        let app = app_with_live_game();
        let texts = app.build_texts();
        // a1 is a dark square, so its file letter must use the light tint.
        assert_eq!(text_named(&texts, "a").color, COORD_ON_DARK);
        // b1 is light, so its letter uses the dark tint.
        assert_eq!(text_named(&texts, "b").color, COORD_ON_LIGHT);
    }

    #[test]
    fn player_info_sits_below_the_board() {
        let app = app_with_live_game();
        let texts = app.build_texts();
        let (_, oy) = app.ui.board_offset();
        let board_bottom = oy + app.ui.square_size() * 8.0;

        let white = text_named(&texts, "White (1500)");
        assert!(white.y >= board_bottom, "player info overlaps the board");
    }
}
