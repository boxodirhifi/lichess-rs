use winit::application::ApplicationHandler;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::window::{Window, WindowId};
use winit::event::{WindowEvent, MouseButton, ElementState, KeyEvent};
use winit::keyboard::{KeyCode, PhysicalKey};
use shakmaty::fen::Fen;
use shakmaty::san::San;
use shakmaty::uci::UciMove;
use shakmaty::FromSetup;
use shakmaty::variant::{
    Antichess, Atomic, Crazyhouse, Horde, KingOfTheHill, RacingKings, ThreeCheck, Variant,
    VariantPosition,
};
use shakmaty::{
    CastlingMode, Chess, Color, KnownOutcome, Move, Outcome, Position, Role, Square,
};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub enum NetEvent {
    FeaturedGame {
        /// Lichess TV channel key (e.g. "atomic"); names the game's variant.
        channel: String,
        fen: String,
        white_name: String,
        white_rating: u32,
        black_name: String,
        black_rating: u32,
        white_secs: u32,
        black_secs: u32,
    },
    FenUpdate {
        channel: String,
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
        winner: Option<Color>,
    },
    /// Incremental Board API state update.
    OnlineState {
        moves: String,
        wtime_ms: u64,
        btime_ms: u64,
        status: String,
        winner: Option<Color>,
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
    board: VariantPosition,
    my_color: Color,
    white_name: String,
    black_name: String,
    white_rating: u32,
    black_rating: u32,
    white_clock: f64,
    black_clock: f64,
    last_update: Instant,
    game_over: bool,
    winner: Option<Color>,
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
    side_to_move: Color,
    last_update: Instant,
    game_over: bool,
}

struct App {
    /// Event proxy for spawning online-play workers; `None` in unit tests.
    proxy: Option<EventLoopProxy<NetEvent>>,
    renderer: Option<renderer::Renderer>,
    window: Option<Arc<Window>>,
    ui: ui::UI,
    board: VariantPosition,
    live_board: Option<VariantPosition>,
    live_info: Option<LiveGameInfo>,
    mode: Mode,
    cursor_pos: (f64, f64),
    side: Color,
    token: Option<String>,
    my_username: Option<String>,
    online: Online,
    online_error: Option<String>,
    /// SAN of every move seen in the current game, oldest first. Rebuilt only
    /// from applied/derived moves; cleared on a new game or a mode switch.
    history: Vec<String>,
}

/// Map a Lichess TV channel key to its shakmaty variant. `Variant::from_ascii`
/// already accepts the Lichess spellings (`kingOfTheHill`, `threeCheck`,
/// `chess960`, …); unknown keys degrade to standard chess.
fn channel_variant(channel: &str) -> Variant {
    Variant::from_ascii(channel.as_bytes()).unwrap_or(Variant::Chess)
}

/// Castling rights in Lichess variant FENs use standard notation; only the
/// Chess960 channel allows rooks anywhere on the backrank.
fn channel_castling_mode(channel: &str) -> CastlingMode {
    match channel_variant(channel) {
        Variant::Chess if channel == "chess960" => CastlingMode::Chess960,
        _ => CastlingMode::Standard,
    }
}

/// Parse a TV FEN into the variant position its channel plays: a concrete
/// `FromSetup` constructor per variant, wrapped in `VariantPosition` for
/// rendering. Variant-only moves (Crazyhouse drops etc.) stay spectate-only.
fn position_from_fen(channel: &str, fen: &str) -> Option<VariantPosition> {
    let setup = Fen::from_ascii(fen.as_bytes()).ok()?.into_setup();
    let mode = channel_castling_mode(channel);
    let pos: VariantPosition = match channel_variant(channel) {
        Variant::Atomic => Atomic::from_setup(setup, mode).ok()?.into(),
        Variant::Antichess => Antichess::from_setup(setup, mode).ok()?.into(),
        Variant::KingOfTheHill => KingOfTheHill::from_setup(setup, mode).ok()?.into(),
        Variant::ThreeCheck => ThreeCheck::from_setup(setup, mode).ok()?.into(),
        Variant::Crazyhouse => Crazyhouse::from_setup(setup, mode).ok()?.into(),
        Variant::RacingKings => RacingKings::from_setup(setup, mode).ok()?.into(),
        Variant::Horde => Horde::from_setup(setup, mode).ok()?.into(),
        Variant::Chess => Chess::from_setup(setup, mode).ok()?.into(),
    };
    Some(pos)
}

/// `Move::to()` reports the rook square for castling; clicks and highlights
/// work on the king's destination square. Variant-only move kinds
/// (Crazyhouse drops, etc.) have no origin square, so the
/// `mv.from() == Some(..)` filters at the click sites already exclude them —
/// variant play stays spectate-only.
fn move_destination(mv: Move, pos: &VariantPosition) -> Square {
    match mv {
        Move::Castle { .. } => mv
            .castling_side()
            .map(|side| side.king_to(pos.turn()))
            .unwrap_or_else(|| mv.to()),
        _ => mv.to(),
    }
}

fn color_name(color: Color) -> &'static str {
    match color {
        Color::White => "White",
        Color::Black => "Black",
    }
}

/// Human text for a variant-specific ending: the variant names how the game
/// ended; the position's outcome names who won.
fn variant_end_text(board: &VariantPosition) -> String {
    let result = match board.outcome() {
        Outcome::Known(KnownOutcome::Decisive { winner }) => {
            format!("{} wins", color_name(winner))
        }
        Outcome::Known(KnownOutcome::Draw) => "draw".to_string(),
        Outcome::Unknown => "game over".to_string(),
    };
    let how = match board.variant() {
        Variant::Atomic => "King exploded",
        Variant::Antichess => "All pieces given away",
        Variant::KingOfTheHill => "King in the centre",
        Variant::ThreeCheck => "Third check delivered",
        Variant::RacingKings => "Race finished",
        Variant::Horde => "Horde defeated",
        Variant::Chess | Variant::Crazyhouse => "Variant game",
    };
    format!("{} - {}", how, result)
}

fn format_clock(secs: f64) -> String {
    let secs = secs.max(0.0) as u64;
    format!("{:02}:{:02}", secs / 60, secs % 60)
}

/// Material values in pawn units; the king is never captured in normal play.
fn role_value(role: Role) -> i32 {
    match role {
        Role::Pawn => 1,
        Role::Knight | Role::Bishop => 3,
        Role::Rook => 5,
        Role::Queen => 9,
        Role::King => 0,
    }
}

/// The reference position for the captured-material count-diff: the variant's
/// own starting position (Horde starts with its pawn wall, Racing Kings without
/// pawns, and so on).
fn variant_start(variant: Variant) -> VariantPosition {
    match variant {
        Variant::Chess => Chess::default().into(),
        Variant::Atomic => Atomic::default().into(),
        Variant::Antichess => Antichess::default().into(),
        Variant::Crazyhouse => Crazyhouse::default().into(),
        Variant::Horde => Horde::default().into(),
        Variant::KingOfTheHill => KingOfTheHill::default().into(),
        Variant::ThreeCheck => ThreeCheck::default().into(),
        Variant::RacingKings => RacingKings::default().into(),
    }
}

fn piece_count(pos: &VariantPosition, color: Color, role: Role) -> u32 {
    Square::ALL
        .iter()
        .filter(|&&sq| {
            pos.board()
                .piece_at(sq)
                .is_some_and(|p| p.color == color && p.role == role)
        })
        .count() as u32
}

/// Captured material, derived statelessly by counting what is missing from the
/// variant's starting position. Because nothing is accumulated, a reconnect or
/// a channel rotation can never leave stale pieces in the tray.
///
/// Approximation: promotions upset the counts (a promoted white pawn reads as a
/// captured white pawn *and* a captured black pawn), so trays can be off by one
/// pawn unit per promotion. Spectate mode sees no moves history to correct it;
/// online games are close because promotions are rare (see AGENTS.md).
struct Captures {
    /// Black pieces White captured, in tray order.
    by_white: Vec<Role>,
    /// White pieces Black captured, in tray order.
    by_black: Vec<Role>,
    /// Material lead in pawn units; positive means White is ahead.
    advantage: i32,
}

impl Captures {
    fn of(pos: &VariantPosition) -> Self {
        let start = variant_start(pos.variant());
        let mut by_white = Vec::new();
        let mut by_black = Vec::new();
        let mut white_points = 0;
        let mut black_points = 0;

        // Pawns first, then ascending value, like a Lichess tray.
        for role in [Role::Pawn, Role::Knight, Role::Bishop, Role::Rook, Role::Queen] {
            let value = role_value(role);
            let black_missing = piece_count(&start, Color::Black, role)
                .saturating_sub(piece_count(pos, Color::Black, role));
            let white_missing = piece_count(&start, Color::White, role)
                .saturating_sub(piece_count(pos, Color::White, role));
            for _ in 0..black_missing {
                by_white.push(role);
                white_points += value;
            }
            for _ in 0..white_missing {
                by_black.push(role);
                black_points += value;
            }
        }

        Self { by_white, by_black, advantage: white_points - black_points }
    }
}

/// The move that took `prev` to `next`, if exactly one legal move explains the
/// new piece placement. Used for TV, where only FENs arrive: a missed move (or
/// a new game) makes every candidate fail the comparison, so nothing is
/// highlighted rather than something wrong.
fn derive_played_move(prev: &VariantPosition, next: &VariantPosition) -> Option<Move> {
    for mv in prev.legal_moves() {
        let mut probe = prev.clone();
        probe.play_unchecked(mv);
        if probe.board() == next.board() {
            return Some(mv);
        }
    }
    None
}

fn derive_last_move(prev: &VariantPosition, next: &VariantPosition) -> Option<(Square, Square)> {
    let mv = derive_played_move(prev, next)?;
    // Variant-only moves (Crazyhouse drops) have no origin square.
    Some((mv.from()?, move_destination(mv, prev)))
}

/// How many captured-piece icons fit in the row from `start` to `end`.
/// Truncates instead of colliding with the clock.
fn tray_fit(start: f32, end: f32, count: usize, icon: f32, gap: f32) -> usize {
    let mut fit = 0;
    let mut x = start;
    while fit < count && x + icon <= end {
        fit += 1;
        x += icon + gap;
    }
    fit
}

/// Clock text with the material lead appended for the leading side.
fn clock_with_advantage(secs: f64, advantage: i32) -> String {
    if advantage > 0 {
        format!("{} +{}", format_clock(secs), advantage)
    } else {
        format_clock(secs)
    }
}

/// One player bar above/below the board (Live and Play modes).
struct PlayerBar {
    label: String,
    /// Clock text, carrying "+N" for the side that leads on material.
    clock: String,
    /// Opponent-coloured pieces this player captured, in tray order.
    captured: Vec<Role>,
    /// Text baseline.
    y: f32,
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
        let checked = if self.board.is_check() {
            self.board.board().king_of(self.side)
        } else {
            None
        };
        self.ui.set_checked_king(checked);
    }

    /// Local play accepts moves until shakmaty says the game is over (or the
    /// 50-move counter has run out).
    fn local_game_over(&self) -> bool {
        self.board.outcome().is_known() || self.board.halfmoves() >= 100
    }

    /// End-of-game banner text, shared by local play and TV spectating.
    /// Derived from shakmaty's outcome and terminal-state predicates — no
    /// local game-status enum: the position is the truth. Variant endings
    /// come first (exploded king, king in the centre, check count, lost
    /// horde …); then the standard checkmate/stalemate/draw branches; the
    /// 50-move rule last.
    fn game_banner(board: &VariantPosition) -> Option<String> {
        if board.is_variant_end() {
            return Some(variant_end_text(board));
        }
        match board.outcome() {
            Outcome::Known(KnownOutcome::Decisive { winner }) => {
                Some(format!("Checkmate - {} wins", color_name(winner)))
            }
            Outcome::Known(KnownOutcome::Draw) => {
                if board.is_stalemate() {
                    Some("Stalemate - Draw".to_string())
                } else if board.is_insufficient_material() {
                    Some("Draw - insufficient material".to_string())
                } else {
                    Some("Draw".to_string())
                }
            }
            Outcome::Unknown => {
                if board.halfmoves() >= 100 {
                    Some("Draw - 50-move rule".to_string())
                } else {
                    None
                }
            }
        }
    }

    fn build_texts(&self) -> Vec<renderer::TextElement> {
        let mut texts = Vec::new();
        self.push_coordinates(&mut texts);
        self.push_player_bars(&mut texts);
        self.push_move_history(&mut texts);
        texts
    }

    /// The two player bars (black above the board, white below) for the mode
    /// being displayed, with clocks and captured material. Local play has no
    /// bars, so it returns `None`.
    fn player_bars(&self) -> Option<[PlayerBar; 2]> {
        let sq = self.ui.square_size();
        if sq <= 0.0 {
            return None;
        }
        let board_px = sq * 8.0;
        let (_, oy) = self.ui.board_offset();
        let font_size = (sq * 0.35).max(14.0);
        let top_y = oy - font_size * 0.4;
        let bottom_y = oy + board_px + font_size * 1.1;

        let (black_label, white_label, black_clock, white_clock, board) = match self.mode {
            Mode::Local => return None,
            Mode::Live => {
                let info = self.live_info.as_ref()?;
                let board = self.live_board.as_ref().unwrap_or(&self.board);
                // Freeze the clocks once the game is over so they don't drain.
                let elapsed = if info.game_over {
                    0.0
                } else {
                    info.last_update.elapsed().as_secs_f64()
                };
                let white_clock = if info.side_to_move == Color::White {
                    (info.white_clock - elapsed).max(0.0)
                } else {
                    info.white_clock
                };
                let black_clock = if info.side_to_move == Color::Black {
                    (info.black_clock - elapsed).max(0.0)
                } else {
                    info.black_clock
                };
                (
                    format!("{} ({})", info.black_name, info.black_rating),
                    format!("{} ({})", info.white_name, info.white_rating),
                    black_clock,
                    white_clock,
                    board,
                )
            }
            Mode::Play => {
                let game = self.online_game()?;
                let elapsed = if game.game_over {
                    0.0
                } else {
                    game.last_update.elapsed().as_secs_f64()
                };
                let white_clock = if game.board.turn() == Color::White {
                    (game.white_clock - elapsed).max(0.0)
                } else {
                    game.white_clock
                };
                let black_clock = if game.board.turn() == Color::Black {
                    (game.black_clock - elapsed).max(0.0)
                } else {
                    game.black_clock
                };
                let mine = |name: &str| {
                    self.my_username
                        .as_deref()
                        .is_some_and(|user| user.eq_ignore_ascii_case(name))
                };
                let star = |name: &str| if mine(name) { " *" } else { "" };
                (
                    format!(
                        "{} ({}){}",
                        game.black_name,
                        game.black_rating,
                        star(&game.black_name)
                    ),
                    format!(
                        "{} ({}){}",
                        game.white_name,
                        game.white_rating,
                        star(&game.white_name)
                    ),
                    black_clock,
                    white_clock,
                    &game.board,
                )
            }
        };

        // Count-diff against the variant's start: black's missing pieces are
        // what White captured, and vice versa.
        let captures = Captures::of(board);
        Some([
            PlayerBar {
                label: black_label,
                clock: clock_with_advantage(black_clock, (-captures.advantage).max(0)),
                captured: captures.by_black,
                y: top_y,
            },
            PlayerBar {
                label: white_label,
                clock: clock_with_advantage(white_clock, captures.advantage.max(0)),
                captured: captures.by_white,
                y: bottom_y,
            },
        ])
    }

    fn push_player_bars(&self, texts: &mut Vec<renderer::TextElement>) {
        let Some(bars) = self.player_bars() else {
            return;
        };
        let sq = self.ui.square_size();
        let font_size = (sq * 0.35).max(14.0);
        let (ox, _) = self.ui.board_offset();
        let left = ox;
        let right = ox + sq * 8.0;
        let white = [1.0, 1.0, 1.0, 1.0];

        for bar in bars {
            texts.push(renderer::TextElement {
                text: bar.label,
                x: left,
                y: bar.y,
                size: font_size,
                color: white,
                align: renderer::TextAlign::Left,
                shadow: false,
            });
            texts.push(renderer::TextElement {
                text: bar.clock,
                x: right,
                y: bar.y,
                size: font_size,
                color: white,
                align: renderer::TextAlign::Right,
                shadow: false,
            });
        }
    }

    /// Captured-piece icons for both bars, laid out after the name and kept
    /// clear of the right-aligned clock. Uses the existing `Scene.pieces` path.
    fn push_captured_icons(
        &self,
        rend: &renderer::Renderer,
        out: &mut Vec<renderer::PieceElement>,
    ) {
        let Some(bars) = self.player_bars() else {
            return;
        };
        let sq = self.ui.square_size();
        let font_size = (sq * 0.35).max(14.0);
        let (ox, _) = self.ui.board_offset();
        let left = ox;
        let right = ox + sq * 8.0;
        let icon = sq * 0.4;
        let gap = sq * 0.015;
        let pad = font_size * 0.6;

        // Black's bar holds the white pieces it captured, white's the black.
        for (bar, color) in bars.iter().zip([Color::White, Color::Black]) {
            let start = left + rend.text_width(&bar.label, font_size) + pad;
            let end = right - rend.text_width(&bar.clock, font_size) - pad;
            let fit = tray_fit(start, end, bar.captured.len(), icon, gap);
            let mut x = start;
            for role in bar.captured.iter().take(fit) {
                out.push(renderer::PieceElement {
                    role: *role,
                    color,
                    x: x + icon * 0.5,
                    y: bar.y - font_size * 0.33,
                    size: icon,
                });
                x += icon + gap;
            }
        }
    }

    /// Where the move-history column starts, and its font size, when the
    /// window leaves at least 300 px of spare space to the right of the board.
    fn history_panel(&self) -> Option<(f32, f32)> {
        let sq = self.ui.square_size();
        if sq <= 0.0 {
            return None;
        }
        let (ox, _) = self.ui.board_offset();
        // The board is centred, so the right margin equals the left offset.
        if ox < 300.0 {
            return None;
        }
        let font_size = (ox * 0.05).clamp(12.0, 24.0);
        Some((ox + sq * 8.0 + 24.0, font_size))
    }

    /// The last ~30 plies as `1. e4 e5` rows in the spare space right of the
    /// board. Purely informational: no scrolling, no input, no PGN export.
    fn push_move_history(&self, texts: &mut Vec<renderer::TextElement>) {
        let Some((x, font_size)) = self.history_panel() else {
            return;
        };
        if self.history.is_empty() {
            return;
        }
        let (_, oy) = self.ui.board_offset();
        let total = self.history.len();
        let mut ply = total.saturating_sub(30);

        // Number from the full history so a truncated window still shows the
        // real move numbers.
        let mut rows: Vec<String> = Vec::new();
        if ply % 2 == 1 {
            rows.push(format!("{}. ... {}", ply / 2 + 1, self.history[ply]));
            ply += 1;
        }
        while ply + 1 < total {
            rows.push(format!(
                "{}. {} {}",
                ply / 2 + 1,
                self.history[ply],
                self.history[ply + 1]
            ));
            ply += 2;
        }
        if ply < total {
            rows.push(format!("{}. {}", ply / 2 + 1, self.history[ply]));
        }

        for (row, line) in rows.into_iter().enumerate() {
            texts.push(renderer::TextElement {
                text: line,
                x,
                y: oy + font_size * (1.0 + row as f32 * 1.45),
                size: font_size,
                color: [0.82, 0.82, 0.82, 1.0],
                align: renderer::TextAlign::Left,
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

    /// Which board clicks operate on: the local one in local play, the
    /// online one while a Board API game is running.
    fn active_board(&mut self) -> Option<&mut VariantPosition> {
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
    fn post_online_move(&mut self, mv: Move) {
        let game_id = self.online_game().map(|game| game.game_id.clone());
        let Some(game_id) = game_id else { return };
        let Some(token) = self.token.clone() else { return };
        let uci = mv.to_uci(CastlingMode::Standard).to_string();
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

    fn finish_promotion(&mut self, role: Role) {
        let Some((from, to, _)) = self.ui.close_promotion() else {
            return;
        };
        let mv = self.active_board().and_then(|board| {
            board
                .legal_moves()
                .into_iter()
                .find(|&mv| {
                    mv.from() == Some(from)
                        && move_destination(mv, board) == to
                        && mv.promotion() == Some(role)
                })
        });
        if let Some(mv) = mv {
            self.apply_move(mv);
        }
        self.ui.clear_selection();
    }

    /// Apply a complete move to the active board: bookkeeping for local play,
    /// and a background POST for online play. Either way, the from/to squares
    /// become the last-move highlight (castling maps to the king's destination).
    fn apply_move(&mut self, mv: Move) {
        let (last, san) = match self.active_board_ref() {
            Some(pos) => (
                mv.from().map(|from| (from, move_destination(mv, pos))),
                Some(San::from_move(pos, mv).to_string()),
            ),
            None => (None, None),
        };
        if self.mode == Mode::Local {
            self.board.play_unchecked(mv);
            self.side = self.board.turn();
            self.refresh_check_highlight();
        } else {
            self.post_online_move(mv);
        }
        if let Some(san) = san {
            self.history.push(san);
        }
        self.ui.set_last_move(last);
    }

    /// The board the display is showing, without mutating it.
    fn active_board_ref(&self) -> Option<&VariantPosition> {
        match self.mode {
            Mode::Local => Some(&self.board),
            Mode::Play => self.online_game().map(|game| &game.board),
            Mode::Live => None,
        }
    }

    /// A new TV position arrived: derive the single move that produced it from
    /// the previously displayed position and highlight it. When nothing matches
    /// (a missed move, a new game), the highlight is cleared instead of wrong.
    fn note_live_move(&mut self, board: &VariantPosition) {
        match self
            .live_board
            .as_ref()
            .and_then(|prev| derive_played_move(prev, board).map(|mv| (mv, prev)))
        {
            Some((mv, prev)) => {
                self.history.push(San::from_move(prev, mv).to_string());
                self.ui
                    .set_last_move(mv.from().map(|from| (from, move_destination(mv, prev))));
            }
            None => self.ui.set_last_move(None),
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
                Some(role) => self.finish_promotion(role),
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
            Mode::Local => !self.local_game_over(),
            Mode::Play => match self.online_game() {
                Some(game) => !game.game_over && game.board.turn() == game.my_color,
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
                            .legal_moves()
                            .into_iter()
                            .filter(|&mv| {
                                mv.from() == Some(from) && move_destination(mv, board) == sq
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let promotion = candidates.iter().any(|mv| mv.promotion().is_some());
                if promotion && my_turn {
                    // Let the player pick the piece before playing the move.
                    let mover_color = self
                        .active_board()
                        .and_then(|board| board.board().piece_at(from))
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
        if let Some(piece) = self
            .active_board()
            .and_then(|board| board.board().piece_at(sq))
        {
            if self.can_select_piece(piece.color) {
                let mut targets: Vec<Square> = self
                    .active_board()
                    .map(|board| {
                        board
                            .legal_moves()
                            .into_iter()
                            .filter(|mv| mv.from() == Some(sq))
                            .map(|mv| move_destination(mv, board))
                            .collect()
                    })
                    .unwrap_or_default();
                targets.sort();
                targets.dedup();
                self.ui.select(sq, targets);
                return;
            }
        }
        self.ui.clear_selection();
    }

    /// Selection rules: local play lets you pick either color; online play
    /// only your own pieces, while it is your turn.
    fn can_select_piece(&self, color: Color) -> bool {
        match self.mode {
            Mode::Local => true,
            Mode::Play => self
                .online_game()
                .map(|game| game.my_color == color)
                .unwrap_or(false),
            Mode::Live => false,
        }
    }

    /// Left button pressed: run the ordinary click handling, then remember a
    /// drag when that press selected a piece. A press that already played a
    /// move (clicking a highlighted target) clears the selection and so never
    /// starts a drag.
    fn handle_mouse_press(&mut self, x: f32, y: f32) {
        self.handle_board_click(x, y);
        if self.mode == Mode::Live {
            return;
        }
        let Some(sq) = self.ui.square_at(x, y) else {
            return;
        };
        if self.ui.selected_square() == Some(sq) {
            self.ui.start_drag(sq, x, y);
        }
    }

    /// Cursor movement; returns true when a redraw is needed so the dragged
    /// piece follows the pointer.
    fn handle_cursor_moved(&mut self, x: f32, y: f32) -> bool {
        self.cursor_pos = (x as f64, y as f64);
        if !self.ui.is_dragging() {
            return false;
        }
        self.ui.drag_to(x, y)
    }

    /// Left button released: a real drag resolves like a click on the release
    /// square — legal target plays (or opens the promotion menu), anything
    /// else clears the selection. A press/release without movement stays a
    /// plain click and keeps the selection.
    fn handle_mouse_release(&mut self, x: f32, y: f32) -> bool {
        if !self.ui.is_dragging() {
            return false;
        }
        if self.ui.drag_moved() {
            self.handle_board_click(x, y);
        }
        self.ui.end_drag();
        true
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
            NetEvent::FeaturedGame { channel, fen, white_name, white_rating, black_name, black_rating, white_secs, black_secs } => {
                let board = match position_from_fen(&channel, &fen) {
                    Some(board) => Some(board),
                    None => {
                        log::warn!("Dropping unparseable TV fen: {}", fen);
                        None
                    }
                };
                let side = board.as_ref().map(|board| board.turn()).unwrap_or(Color::White);
                if let Some(board) = board {
                    self.live_board = Some(board);
                }
                // A new featured game starts with no previous move.
                self.ui.set_last_move(None);
                self.history.clear();
                self.live_info = Some(LiveGameInfo {
                    white_name,
                    black_name,
                    white_rating,
                    black_rating,
                    white_clock: white_secs as f64,
                    black_clock: black_secs as f64,
                    side_to_move: side,
                    last_update: Instant::now(),
                    game_over: false,
                });
            }
            NetEvent::FenUpdate { channel, fen, white_secs, black_secs } => {
                let board = match position_from_fen(&channel, &fen) {
                    Some(board) => board,
                    None => {
                        log::warn!("Dropping unparseable TV fen: {}", fen);
                        return;
                    }
                };
                let side = board.turn();
                // Variant games can end without a standard checkmate/stalemate
                // (exploded king, king in the centre, lost horde, check count).
                let terminal =
                    board.outcome().is_known() || board.is_variant_end() || board.halfmoves() >= 100;
                // Derive the played move from the previously displayed board
                // before overwriting it.
                self.note_live_move(&board);
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
                    Color::White
                } else {
                    Color::Black
                };
                let mut board = match initial_fen.as_deref().unwrap_or("startpos") {
                    "startpos" => VariantPosition::default(),
                    fen => position_from_fen("standard", fen).unwrap_or_default(),
                };
                replay_uci(&mut board, &moves);
                // A fresh online game starts without a last-move highlight.
                self.ui.set_last_move(None);
                self.history.clear();
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
                let mut advanced = None;
                if let Online::Active(game) = &mut self.online {
                    // The server echoes the whole move list; only the unseen
                    // tail advances the board. Compare before/after and derive
                    // the move rather than guessing.
                    let prev = game.board.clone();
                    replay_uci(&mut game.board, &moves);
                    if prev.board() != game.board.board() {
                        advanced = Some(derive_last_move(&prev, &game.board));
                    }
                    game.white_clock = wtime_ms as f64 / 1000.0;
                    game.black_clock = btime_ms as f64 / 1000.0;
                    game.last_update = Instant::now();
                    game.game_over = status != "started";
                    game.winner = winner;
                }
                if let Some(last) = advanced {
                    self.ui.set_last_move(last);
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
                // Dragging needs a redraw on every move (the piece follows the
                // cursor); promotion hover is the other cursor-driven visual.
                let dragging = self.handle_cursor_moved(position.x as f32, position.y as f32);
                needs_redraw = dragging || self.ui.promotion_open();
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                let (x, y) = self.cursor_pos;
                self.handle_mouse_press(x as f32, y as f32);
                needs_redraw = true;
            }
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => {
                let (x, y) = self.cursor_pos;
                needs_redraw = self.handle_mouse_release(x as f32, y as f32);
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
                        self.ui.set_last_move(None);
                        self.history.clear();
                        needs_redraw = true;
                    }
                    KeyCode::KeyP => {
                        self.mode = Mode::Local;
                        self.refresh_check_highlight();
                        self.ui.set_last_move(None);
                        self.history.clear();
                        needs_redraw = true;
                    }
                    KeyCode::KeyC => {
                        self.mode = Mode::Play;
                        self.ui.clear_selection();
                        self.ui.clear_promotion();
                        self.ui.set_last_move(None);
                        self.history.clear();
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
                let mut pieces: Vec<renderer::PieceElement> = self.ui.promotion_choices().iter().map(|c| {
                    renderer::PieceElement {
                        role: c.role,
                        color: c.color,
                        x: c.rect.x + c.rect.w * 0.5,
                        y: c.rect.y + c.rect.h * 0.5,
                        size: c.rect.h,
                    }
                }).collect();
                // Captured trays live in the player bars (Live/Play only); they
                // are ordinary extra pieces, no new pipeline.
                if let Some(rend) = self.renderer.as_ref() {
                    self.push_captured_icons(rend, &mut pieces);
                }
                let banner: Option<String> = match self.mode {
                    Mode::Local => App::game_banner(&self.board),
                    Mode::Live => {
                        // TV games end either through the standard outcomes,
                        // the window's 50-move counter, or a variant ending;
                        // show the banner text once the worker freezes them.
                        if self.live_info.as_ref().is_some_and(|info| info.game_over) {
                            self.live_board.as_ref().and_then(App::game_banner)
                        } else {
                            None
                        }
                    }
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

                // Once a press turns into a real drag, lift the piece off its
                // square and let it follow the cursor. A plain click keeps the
                // board untouched (the piece is only hidden from the moment
                // the pointer leaves the click slop).
                let dragged = if self.ui.is_dragging() && self.ui.drag_moved() {
                    self.ui.dragging_piece(board).map(|(from, role, color)| {
                        (
                            from,
                            renderer::PieceElement {
                                role,
                                color,
                                x: self.cursor_pos.0 as f32,
                                y: self.cursor_pos.1 as f32,
                                size: self.ui.square_size(),
                            },
                        )
                    })
                } else {
                    None
                };

                if let Some(rend) = self.renderer.as_mut() {
                    let scene = renderer::Scene {
                        quads: &quads,
                        board,
                        board_offset: self.ui.board_offset(),
                        square_size: self.ui.square_size(),
                        banner,
                        texts: &texts,
                        pieces: &pieces,
                        dragged,
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

/// TV channels the worker rotates through. The variant channels make sure
/// every shakmaty variant lights up during a cycle; each stream is held
/// open for `TV_CHANNEL_DWELL` and then rotated.
const TV_CHANNELS: &[&str] = &[
    "best",
    "atomic",
    "horde",
    "crazyhouse",
    "kingOfTheHill",
    "threeCheck",
    "antichess",
    "racingKings",
    "chess960",
];

/// How long one TV channel is watched before rotating to the next.
const TV_CHANNEL_DWELL: Duration = Duration::from_secs(30);

fn lichess_worker(proxy: EventLoopProxy<NetEvent>) {
    let mut channel_idx = 0usize;
    loop {
        let channel = TV_CHANNELS[channel_idx % TV_CHANNELS.len()];
        channel_idx += 1;
        let started = Instant::now();
        log::info!("Connecting to Lichess TV stream (channel {})...", channel);
        let result = lichess::stream_tv_feed(Some(channel), TV_CHANNEL_DWELL, |event| {
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
                        channel: channel.to_string(),
                        fen: data.fen,
                        white_name, white_rating,
                        black_name, black_rating,
                        white_secs, black_secs,
                    });
                }
                lichess::TvStreamEvent::Fen(data) => {
                    let _ = proxy.send_event(NetEvent::FenUpdate {
                        channel: channel.to_string(),
                        fen: data.fen,
                        white_secs: data.wc,
                        black_secs: data.bc,
                    });
                }
            }
        });

        match result {
            Ok(()) => log::info!("TV stream (channel {}) rotated", channel),
            Err(e) => log::warn!("TV stream (channel {}) ended: {}", channel, e),
        }

        // Don't hot-loop when a channel is refused or closes at once.
        if started.elapsed() < Duration::from_secs(1) {
            std::thread::sleep(Duration::from_secs(1));
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
fn replay_uci(board: &mut VariantPosition, moves: &str) {
    for uci in moves.split_whitespace() {
        let Ok(parsed) = UciMove::from_ascii(uci.as_bytes()) else {
            log::warn!("UCI replay stopped at {}", uci);
            break;
        };
        match parsed.to_move(board) {
            Ok(mv) => board.play_unchecked(mv),
            Err(_) => {
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
        "white" => Some(Color::White),
        "black" => Some(Color::Black),
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
        board: VariantPosition::default(),
        live_board: None,
        live_info: None,
        side: Color::White,
        mode: Mode::Live,
        cursor_pos: (0.0, 0.0),
        token: None,
        my_username: None,
        online: Online::Idle,
        online_error: None,
        history: Vec::new(),
    };
    event_loop.run_app(&mut app).unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_from_fen(fen: &str) -> App {
        let board = position_from_fen("standard", fen).expect("valid test FEN");
        let side = board.turn();
        let mut app = App {
            proxy: None,
            renderer: None,
            window: None,
            ui: ui::UI::new(),
            board,
            live_board: None,
            live_info: None,
            side,
            mode: Mode::Local,
            cursor_pos: (0.0, 0.0),
            token: None,
            my_username: None,
            online: Online::Idle,
            online_error: None,
            history: Vec::new(),
        };
        app.ui.layout(800.0, 600.0);
        app
    }

    /// A board with a white pawn on the 7th rank, ready to promote.
    fn app_with_promoting_pawn() -> App {
        app_from_fen("8/4P3/8/8/8/8/8/4K2k w - - 0 1")
    }

    /// Piece counts per colour on a variant board (sane-count assertions).
    fn piece_counts(board: &VariantPosition) -> (u32, u32) {
        Square::ALL
            .into_iter()
            .filter_map(|sq| board.board().piece_at(sq))
            .fold((0, 0), |(white, black), piece| match piece.color {
                Color::White => (white + 1, black),
                Color::Black => (white, black + 1),
            })
    }

    #[test]
    fn tv_channel_keys_pick_their_variants() {
        assert_eq!(channel_variant("atomic"), Variant::Atomic);
        assert_eq!(channel_variant("horde"), Variant::Horde);
        assert_eq!(channel_variant("crazyhouse"), Variant::Crazyhouse);
        assert_eq!(channel_variant("antichess"), Variant::Antichess);
        assert_eq!(channel_variant("kingOfTheHill"), Variant::KingOfTheHill);
        assert_eq!(channel_variant("threeCheck"), Variant::ThreeCheck);
        assert_eq!(channel_variant("racingKings"), Variant::RacingKings);
        // Chess960 is standard chess with different castling encoding.
        assert_eq!(channel_variant("chess960"), Variant::Chess);
        assert_eq!(channel_castling_mode("chess960"), CastlingMode::Chess960);
        assert_eq!(channel_castling_mode("atomic"), CastlingMode::Standard);
        // Unknown keys degrade to standard chess instead of failing.
        assert_eq!(channel_variant("mystery"), Variant::Chess);
    }

    #[test]
    fn tv_games_build_their_variants() {
        // A real Lichess TV horde game: a pawn horde vs a small army.
        let horde = position_from_fen(
            "horde",
            "4k1nr/r7/5PP1/4P3/1P1P1PP1/1PP3PP/PPP4P/1PPP1Pq1 b k - 0 33",
        )
        .expect("horde fen");
        assert_eq!(horde.variant(), Variant::Horde);
        let (white, black) = piece_counts(&horde);
        assert_eq!(white, 19, "the horde is pawns only");
        assert_eq!(black, 5);
        let _ = horde.legal_moves();

        // A real Lichess TV atomic game.
        let atomic = position_from_fen(
            "atomic",
            "r2k1b1r/pp3Bp1/2p2p2/4p2p/3PP3/2P2P2/PP4P1/R4K2 b - - 0 15",
        )
        .expect("atomic fen");
        assert_eq!(atomic.variant(), Variant::Atomic);
        let (white, black) = piece_counts(&atomic);
        assert_eq!(white, 10);
        assert_eq!(black, 11);
        let _ = atomic.legal_moves();

        // Crazyhouse positions carry pockets, which nothing but rules use
        // (UI and rendering read the board only) — spectate-only.
        let crazy = position_from_fen(
            "crazyhouse",
            "1Q~2bq2/4b1kp/3pPp1n/6p1/4b2P/4P1R1/PPP2PP1/R2QK1NR/NNBRppp w KQ - 0 32",
        )
        .expect("crazyhouse fen");
        assert_eq!(crazy.variant(), Variant::Crazyhouse);
        assert!(crazy.pockets().is_some());
        let _ = crazy.legal_moves();
    }

    #[test]
    fn atomic_positions_stay_out_of_standard_chess() {
        // Kings on a8/b7 are adjacent: legal in Atomic (capturing the king
        // ends the game with an explosion), illegal as standard chess.
        let adjacent = "k7/1K6/8/8/8/8/8/8 w - - 0 1";
        assert!(position_from_fen("atomic", adjacent).is_some());
        assert!(position_from_fen("standard", adjacent).is_none());
    }

    #[test]
    fn variant_end_banner_names_the_variant() {
        // White king already stands on one of the four centre (hill) squares.
        let board = position_from_fen("kingOfTheHill", "8/8/8/3K4/8/8/8/k7 w - - 0 1")
            .expect("king of the hill fen");
        assert!(board.is_variant_end());
        let text = App::game_banner(&board).expect("variant banner");
        assert!(
            text.starts_with("King in the centre - White wins"),
            "banner: {text}"
        );
    }

    /// Center of a square in window coordinates.
    fn square_center(app: &App, sq: Square) -> (f32, f32) {
        let (ox, oy) = app.ui.board_offset();
        let s = app.ui.square_size();
        (
            ox + (sq.file().to_u32() as f32 + 0.5) * s,
            oy + (7.0 - sq.rank().to_u32() as f32 + 0.5) * s,
        )
    }

    /// Center of a promotion menu cell.
    fn choice_center(app: &App, role: Role) -> (f32, f32) {
        let cell = app
            .ui
            .promotion_choices()
            .iter()
            .find(|c| c.role == role)
            .expect("promotion choice missing");
        (
            cell.rect.x + cell.rect.w * 0.5,
            cell.rect.y + cell.rect.h * 0.5,
        )
    }

    #[test]
    fn promoting_opens_the_menu_instead_of_moving() {
        let mut app = app_with_promoting_pawn();
        let e7 = Square::E7;
        let e8 = Square::E8;

        let (x, y) = square_center(&app, e7);
        app.handle_board_click(x, y);
        assert_eq!(app.ui.selected_square(), Some(e7));

        let (x, y) = square_center(&app, e8);
        app.handle_board_click(x, y);

        assert!(app.ui.promotion_open(), "menu should be open after the click");
        assert_eq!(app.ui.promotion_from(), Some(e7));
        // The pawn has not moved yet.
        assert!(matches!(
            app.board.board().piece_at(e7).map(|p| p.role),
            Some(Role::Pawn)
        ));
        assert!(app.board.board().piece_at(e8).is_none());
        // The game is still White's move.
        assert_eq!(app.side, Color::White);
    }

    #[test]
    fn picking_a_choice_plays_that_piece() {
        let mut app = app_with_promoting_pawn();
        let e7 = Square::E7;
        let e8 = Square::E8;

        let (x, y) = square_center(&app, e7);
        app.handle_board_click(x, y);
        let (x, y) = square_center(&app, e8);
        app.handle_board_click(x, y);
        assert!(app.ui.promotion_open());

        let (x, y) = choice_center(&app, Role::Knight);
        app.handle_board_click(x, y);

        assert!(!app.ui.promotion_open(), "menu should close after picking");
        assert!(app.board.board().piece_at(e7).is_none());
        assert_eq!(
            app.board.board().piece_at(e8).map(|p| (p.role, p.color)),
            Some((Role::Knight, Color::White))
        );
        assert_eq!(app.side, Color::Black);
    }

    #[test]
    fn every_choice_produces_its_piece() {
        for kind in [
            Role::Queen,
            Role::Rook,
            Role::Bishop,
            Role::Knight,
        ] {
            let mut app = app_with_promoting_pawn();
            let e7 = Square::E7;
            let e8 = Square::E8;

            let (x, y) = square_center(&app, e7);
            app.handle_board_click(x, y);
            let (x, y) = square_center(&app, e8);
            app.handle_board_click(x, y);

            let (x, y) = choice_center(&app, kind);
            app.handle_board_click(x, y);

            assert_eq!(
                app.board.board().piece_at(e8).map(|p| p.role),
                Some(kind),
                "wrong piece for {kind:?}"
            );
        }
    }

    #[test]
    fn clicking_away_from_the_menu_dismisses_it() {
        let mut app = app_with_promoting_pawn();
        let e7 = Square::E7;
        let e8 = Square::E8;

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
            app.board.board().piece_at(e7).map(|p| p.role),
            Some(Role::Pawn)
        ));
        assert_eq!(app.ui.selected_square(), Some(e7));
    }

    #[test]
    fn closing_the_menu_keeps_the_pawn_selected() {
        let mut app = app_with_promoting_pawn();
        let e7 = Square::E7;
        let e8 = Square::E8;

        let (x, y) = square_center(&app, e7);
        app.handle_board_click(x, y);
        let (x, y) = square_center(&app, e8);
        app.handle_board_click(x, y);
        assert!(app.ui.promotion_open());

        // What Escape runs; the pawn stays put and selected.
        app.ui.close_promotion();
        assert!(!app.ui.promotion_open());
        assert!(matches!(
            app.board.board().piece_at(e7).map(|p| p.role),
            Some(Role::Pawn)
        ));
        assert_eq!(app.ui.selected_square(), Some(e7));
    }

    #[test]
    fn replay_uci_applies_moves_with_side_to_move() {
        let mut board = VariantPosition::default();
        replay_uci(&mut board, "e2e4 g8f6 b1c3");
        assert_eq!(board.turn(), Color::Black);
        assert_eq!(
            board.board().piece_at(Square::C3).map(|p| p.role),
            Some(Role::Knight)
        );

        // Standard UCI castling is king-to-destination.
        let mut board = VariantPosition::default();
        replay_uci(&mut board, "e2e4 e7e5 g1f3 b8c6 f1c4 g8f6 e1g1");
        assert_eq!(
            board.board().piece_at(Square::G1).map(|p| p.role),
            Some(Role::King)
        );
        assert_eq!(board.turn(), Color::Black);

        // Unknown moves stop the replay instead of panicking.
        let mut board = VariantPosition::default();
        replay_uci(&mut board, "e2e4 nonsense e7e5");
        assert_eq!(board.turn(), Color::Black);
    }

    #[test]
    fn clicking_the_king_destination_castles() {
        let mut app = app_from_fen("4k3/8/8/8/8/8/8/R3K2R w KQ - 0 1");

        let (x, y) = square_center(&app, Square::E1);
        app.handle_board_click(x, y);
        let (x, y) = square_center(&app, Square::G1);
        app.handle_board_click(x, y);

        assert_eq!(
            app.board.board().piece_at(Square::G1).map(|p| p.role),
            Some(Role::King)
        );
        assert_eq!(
            app.board.board().piece_at(Square::F1).map(|p| p.role),
            Some(Role::Rook)
        );
        assert!(app.board.board().piece_at(Square::H1).is_none());
        assert_eq!(app.side, Color::Black);
    }

    #[test]
    fn ordinary_still_moves_immediately() {
        let mut app = app_from_fen("rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1");
        let d2 = Square::D2;
        let d4 = Square::D4;

        // A pawn on d2 is not promoting, so a two-square push just happens.
        let (x, y) = square_center(&app, d2);
        app.handle_board_click(x, y);
        let (x, y) = square_center(&app, d4);
        app.handle_board_click(x, y);

        assert!(!app.ui.promotion_open());
        assert!(app.board.board().piece_at(d4).is_some());
        assert_eq!(app.side, Color::Black);
    }

    #[test]
    fn drag_start_selects_and_lifts_the_piece() {
        let mut app = app_from_fen("rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1");
        let e2 = Square::E2;

        let (x, y) = square_center(&app, e2);
        app.handle_mouse_press(x, y);

        assert_eq!(app.ui.selected_square(), Some(e2));
        assert_eq!(app.ui.dragging_square(), Some(e2));
        assert!(app.ui.is_dragging());
        assert!(!app.ui.drag_moved(), "the press alone is not a drag yet");
        assert_eq!(
            app.ui.dragging_piece(&app.board),
            Some((e2, Role::Pawn, Color::White))
        );
        // The legal targets are already highlighted while dragging.
        assert!(app.ui.is_move_target(Square::E4));
    }

    #[test]
    fn drag_release_over_a_legal_target_plays_the_move() {
        let mut app = app_from_fen("rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1");
        let e2 = Square::E2;
        let e4 = Square::E4;

        let (x, y) = square_center(&app, e2);
        app.handle_mouse_press(x, y);
        let (x, y) = square_center(&app, e4);
        assert!(app.handle_cursor_moved(x, y), "the followed cursor needs a redraw");
        assert!(app.ui.drag_moved());

        assert!(app.handle_mouse_release(x, y));
        assert_eq!(
            app.board.board().piece_at(e4).map(|p| (p.role, p.color)),
            Some((Role::Pawn, Color::White))
        );
        assert!(app.board.board().piece_at(e2).is_none());
        assert_eq!(app.side, Color::Black);
        assert!(!app.ui.is_dragging());
        assert_eq!(app.ui.selected_square(), None);
    }

    #[test]
    fn drag_release_elsewhere_cancels_the_move() {
        let mut app = app_from_fen("rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1");
        let e2 = Square::E2;

        // Released on an unrelated square: the pawn goes back and the
        // selection clears.
        let (px, py) = square_center(&app, e2);
        app.handle_mouse_press(px, py);
        let (x, y) = square_center(&app, Square::A3);
        app.handle_cursor_moved(x, y);
        app.handle_mouse_release(x, y);

        assert_eq!(
            app.board.board().piece_at(e2).map(|p| p.role),
            Some(Role::Pawn)
        );
        assert!(app.board.board().piece_at(Square::A3).is_none());
        assert_eq!(app.side, Color::White);
        assert!(!app.ui.is_dragging());
        assert_eq!(app.ui.selected_square(), None);

        // Released outside the board does the same.
        let (px, py) = square_center(&app, e2);
        app.handle_mouse_press(px, py);
        app.handle_cursor_moved(2.0, 2.0);
        app.handle_mouse_release(2.0, 2.0);

        assert!(app.board.board().piece_at(e2).is_some());
        assert!(!app.ui.is_dragging());
        assert_eq!(app.ui.selected_square(), None);
    }

    #[test]
    fn a_click_is_still_a_click() {
        let mut app = app_from_fen("rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1");
        let e2 = Square::E2;
        let e4 = Square::E4;

        // Press and release in place: click-to-select keeps working and the
        // drag must not clear the selection.
        let (x, y) = square_center(&app, e2);
        app.handle_mouse_press(x, y);
        assert!(app.handle_mouse_release(x, y));
        assert_eq!(app.ui.selected_square(), Some(e2));
        assert!(!app.ui.is_dragging());

        // The second click on a highlighted square plays the move, exactly as
        // before, and does not start a drag of the released piece.
        let (x, y) = square_center(&app, e4);
        app.handle_mouse_press(x, y);
        assert!(app.board.board().piece_at(e4).is_some());
        assert!(!app.ui.is_dragging());
        app.handle_mouse_release(x, y);
        assert!(!app.ui.is_dragging());
        assert_eq!(app.ui.selected_square(), None);
    }

    #[test]
    fn dragging_a_pawn_to_the_last_rank_opens_the_promotion_menu() {
        let mut app = app_with_promoting_pawn();
        let e7 = Square::E7;
        let e8 = Square::E8;

        let (x, y) = square_center(&app, e7);
        app.handle_mouse_press(x, y);
        let (x, y) = square_center(&app, e8);
        app.handle_cursor_moved(x, y);
        app.handle_mouse_release(x, y);

        assert!(app.ui.promotion_open(), "menu opens on release, as with a click");
        assert_eq!(app.ui.promotion_from(), Some(e7));
        assert!(!app.ui.is_dragging());
        // The pawn has not moved yet: the choice picks the promotion piece.
        assert!(matches!(
            app.board.board().piece_at(e7).map(|p| p.role),
            Some(Role::Pawn)
        ));
        assert!(app.board.board().piece_at(e8).is_none());

        let (x, y) = choice_center(&app, Role::Knight);
        app.handle_board_click(x, y);
        assert!(!app.ui.promotion_open());
        assert_eq!(
            app.board.board().piece_at(e8).map(|p| (p.role, p.color)),
            Some((Role::Knight, Color::White))
        );
    }

    #[test]
    fn dragging_does_nothing_in_live_mode() {
        let mut app = app_with_live_game();
        let e2 = Square::E2;

        let (x, y) = square_center(&app, e2);
        app.handle_mouse_press(x, y);
        assert!(!app.ui.is_dragging());
        assert_eq!(app.ui.selected_square(), None);

        let (x, y) = square_center(&app, Square::E4);
        assert!(!app.handle_cursor_moved(x, y));
        assert!(!app.handle_mouse_release(x, y));
        assert!(!app.ui.is_dragging());
        assert!(app.live_board.as_ref().expect("live board").board().piece_at(e2).is_some());
        assert!(app.live_board.as_ref().expect("live board").board().piece_at(Square::E4).is_none());
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
        app.live_board = Some(VariantPosition::default());
        app.live_info = Some(LiveGameInfo {
            white_name: "White".to_string(),
            black_name: "Black".to_string(),
            white_rating: 1500,
            black_rating: 1600,
            white_clock: 300.0,
            black_clock: 300.0,
            side_to_move: Color::White,
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

    /// Play a UCI move on a position (test helper).
    fn play_uci(pos: &mut VariantPosition, uci: &str) {
        let mv = UciMove::from_ascii(uci.as_bytes())
            .expect("valid uci")
            .to_move(pos)
            .expect("legal move");
        pos.play_unchecked(mv);
    }

    #[test]
    fn last_move_derivation_handles_every_move_kind() {
        let start = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1";

        // Normal push.
        let prev = position_from_fen("standard", start).expect("start fen");
        let mut next = prev.clone();
        play_uci(&mut next, "e2e4");
        assert_eq!(derive_last_move(&prev, &next), Some((Square::E2, Square::E4)));

        // Capture.
        let prev = position_from_fen(
            "standard",
            "rnbqkbnr/ppp1pppp/8/3p4/4P3/8/PPPP1PPP/RNBQKBNR w KQkq - 0 2",
        )
        .expect("capture fen");
        let mut next = prev.clone();
        play_uci(&mut next, "e4d5");
        assert_eq!(derive_last_move(&prev, &next), Some((Square::E4, Square::D5)));

        // Castling maps to the king's destination, not the rook's.
        let prev =
            position_from_fen("standard", "r3k2r/8/8/8/8/8/8/R3K2R w KQkq - 0 1").expect("castle fen");
        let mut next = prev.clone();
        play_uci(&mut next, "e1g1");
        assert_eq!(derive_last_move(&prev, &next), Some((Square::E1, Square::G1)));

        // En passant.
        let prev = position_from_fen("standard", "4k3/8/8/3pP3/8/8/8/4K3 w - d6 0 1")
            .expect("en passant fen");
        let mut next = prev.clone();
        play_uci(&mut next, "e5d6");
        assert_eq!(derive_last_move(&prev, &next), Some((Square::E5, Square::D6)));

        // Promotion.
        let prev =
            position_from_fen("standard", "8/4P3/8/8/8/8/8/4K2k w - - 0 1").expect("promotion fen");
        let mut next = prev.clone();
        play_uci(&mut next, "e7e8q");
        assert_eq!(derive_last_move(&prev, &next), Some((Square::E7, Square::E8)));
    }

    #[test]
    fn tv_positions_highlight_the_derived_move_and_clear_on_a_gap() {
        let mut app = app_from_fen("rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1");
        app.mode = Mode::Live;
        let start = VariantPosition::default();
        app.live_board = Some(start.clone());

        // One legal move away: the highlight is exact.
        let mut after_e4 = start;
        play_uci(&mut after_e4, "e2e4");
        app.note_live_move(&after_e4);
        assert_eq!(app.ui.last_move(), Some((Square::E2, Square::E4)));
        assert_eq!(app.history, vec!["e4"]);
        app.live_board = Some(after_e4.clone());

        // Three plies arrived between events: nothing matches, so no tint
        // rather than a wrong one.
        let mut far = after_e4;
        play_uci(&mut far, "e7e5");
        play_uci(&mut far, "g1f3");
        app.note_live_move(&far);
        assert_eq!(app.ui.last_move(), None);
        // The un-derived moves are not invented in the history either.
        assert_eq!(app.history, vec!["e4"]);
    }

    #[test]
    fn locally_played_moves_become_the_last_move_highlight() {
        let mut app = app_from_fen("rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1");
        let (x, y) = square_center(&app, Square::E2);
        app.handle_board_click(x, y);
        let (x, y) = square_center(&app, Square::E4);
        app.handle_board_click(x, y);
        assert_eq!(app.ui.last_move(), Some((Square::E2, Square::E4)));

        // Castling highlights the king's origin and destination.
        let mut app = app_from_fen("4k3/8/8/8/8/8/8/R3K2R w KQ - 0 1");
        let (x, y) = square_center(&app, Square::E1);
        app.handle_board_click(x, y);
        let (x, y) = square_center(&app, Square::G1);
        app.handle_board_click(x, y);
        assert_eq!(app.ui.last_move(), Some((Square::E1, Square::G1)));

        // Promoting records the pawn move onto the last rank.
        let mut app = app_with_promoting_pawn();
        let (x, y) = square_center(&app, Square::E7);
        app.handle_board_click(x, y);
        let (x, y) = square_center(&app, Square::E8);
        app.handle_board_click(x, y);
        let (x, y) = choice_center(&app, Role::Queen);
        app.handle_board_click(x, y);
        assert_eq!(app.ui.last_move(), Some((Square::E7, Square::E8)));
    }

    #[test]
    fn captured_material_is_a_stateless_count_diff() {
        // White lost a rook on a1, Black lost a knight on g8: Black leads by 2.
        let board = position_from_fen(
            "standard",
            "rnbqkb1r/pppppppp/8/8/8/8/PPPPPPPP/1NBQKBNR w - - 0 1",
        )
        .expect("capture fen");
        let caps = Captures::of(&board);
        assert_eq!(caps.by_white, vec![Role::Knight]);
        assert_eq!(caps.by_black, vec![Role::Rook]);
        assert_eq!(caps.advantage, -2);

        // An untouched starting board has empty trays and level material.
        let start = VariantPosition::default();
        let caps = Captures::of(&start);
        assert!(caps.by_white.is_empty());
        assert!(caps.by_black.is_empty());
        assert_eq!(caps.advantage, 0);
    }

    #[test]
    fn leading_side_shows_material_next_to_its_clock() {
        let mut app = app_with_live_game();
        // Black's queen and h8 rook are gone: White leads by 14.
        app.live_board = Some(
            position_from_fen(
                "standard",
                "rnb1kbn1/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQ - 0 1",
            )
            .expect("capture fen"),
        );
        // Black to move keeps White's clock frozen at its server value, so the
        // displayed text is deterministic.
        app.live_info.as_mut().expect("live info").side_to_move = Color::Black;

        let bars = app.player_bars().expect("live bars");
        // Black leads nothing; no "+N" appended to its ticking clock.
        assert!(!bars[0].clock.contains('+'));
        assert!(bars[0].captured.is_empty());
        // White's frozen clock shows the lead.
        assert_eq!(bars[1].clock, "05:00 +14");
        assert_eq!(bars[1].captured, vec![Role::Rook, Role::Queen]);

        // The texts include the decorated clock.
        let texts = app.build_texts();
        assert!(texts.iter().any(|t| t.text == "05:00 +14"));
    }

    #[test]
    fn play_bars_carry_captures_and_advantage() {
        let mut app = app_from_fen("rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1");
        app.mode = Mode::Play;
        app.my_username = Some("me".to_string());
        app.online = Online::Active(Box::new(OnlineGame {
            game_id: "test".to_string(),
            board: position_from_fen(
                "standard",
                "rnb1kbn1/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQ - 0 1",
            )
            .expect("capture fen"),
            my_color: Color::White,
            white_name: "me".to_string(),
            black_name: "them".to_string(),
            white_rating: 1500,
            black_rating: 1600,
            white_clock: 300.0,
            black_clock: 300.0,
            last_update: Instant::now(),
            game_over: true,
            winner: None,
        }));

        let bars = app.player_bars().expect("play bars");
        assert_eq!(bars[0].label, "them (1600)");
        assert_eq!(bars[1].label, "me (1500) *");
        assert_eq!(bars[1].clock, "05:00 +14");
        assert!(bars[0].captured.is_empty());
        assert_eq!(bars[1].captured, vec![Role::Rook, Role::Queen]);
    }

    #[test]
    fn local_play_has_no_player_bars() {
        let app = app_from_fen("rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1");
        assert_eq!(app.mode, Mode::Local);
        assert!(app.player_bars().is_none());
    }

    #[test]
    fn san_history_follows_local_play() {
        let mut app = app_from_fen("rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1");
        for sq in [
            Square::E2,
            Square::E4,
            Square::E7,
            Square::E5,
            Square::G1,
            Square::F3,
        ] {
            let (x, y) = square_center(&app, sq);
            app.handle_board_click(x, y);
        }
        assert_eq!(app.history, vec!["e4", "e5", "Nf3"]);
    }

    #[test]
    fn move_history_panel_needs_spare_width() {
        let mut app = app_from_fen("rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1");
        // 800x600 leaves only 160 px each side of the board: no column.
        assert!(app.history_panel().is_none());

        // 1400x800 leaves 380 px: the column opens beside the board.
        app.ui.layout(1400.0, 800.0);
        let (x, font_size) = app.history_panel().expect("wide window shows history");
        let board_right = app.ui.board_offset().0 + app.ui.square_size() * 8.0;
        assert!(x > board_right);
        assert!(font_size >= 12.0);

        // Rows pair up plies: 1. e4 e5.
        for sq in [Square::E2, Square::E4, Square::E7, Square::E5] {
            let (px, py) = square_center(&app, sq);
            app.handle_board_click(px, py);
        }
        let texts = app.build_texts();
        assert!(texts.iter().any(|t| t.text == "1. e4 e5"));
    }

    #[test]
    fn move_history_shows_only_the_latest_plies() {
        let mut app = app_from_fen("rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1");
        app.ui.layout(1400.0, 800.0);
        for i in 0..40 {
            app.history.push(format!("m{i}"));
        }
        let texts = app.build_texts();
        // 40 plies, keep the last 30: rows start at move 6.
        assert!(texts.iter().any(|t| t.text == "6. m10 m11"));
        assert!(texts.iter().any(|t| t.text == "20. m38 m39"));
        assert!(!texts.iter().any(|t| t.text.starts_with("1. m0")));
    }

    #[test]
    fn captured_tray_truncates_when_space_is_tight() {
        // 9 icons of 20 px, 2 px apart, fit into 200 px.
        assert_eq!(tray_fit(0.0, 200.0, 10, 20.0, 2.0), 9);
        // A tight row keeps only the first two.
        assert_eq!(tray_fit(10.0, 60.0, 10, 20.0, 2.0), 2);
        // No room at all: nothing is drawn rather than colliding with the clock.
        assert_eq!(tray_fit(100.0, 100.0, 3, 20.0, 2.0), 0);
        assert_eq!(tray_fit(150.0, 100.0, 3, 20.0, 2.0), 0);
        // Fewer pieces than the row can hold: all of them.
        assert_eq!(tray_fit(0.0, 200.0, 2, 20.0, 2.0), 2);
    }
}
