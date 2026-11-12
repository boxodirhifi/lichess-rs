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
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

mod config;
mod permissions;

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
    OnlineAccount {
        username: String,
    },
    /// The challenge exists; the opponent still has to accept it.
    OnlineChallengeCreated {
        challenge_id: String,
    },
    /// A Board API game (accepted challenge) is set up.
    OnlineFull(Box<lichess::GameFull>),
    /// Incremental Board API state update.
    OnlineState(lichess::GameState),
    OnlineFailed {
        message: String,
    },
    /// A played move could not be sent or was not accepted; roll the optimistic
    /// board back to the last server position.
    OnlineMoveRejected {
        message: String,
    },
    /// A non-fatal action result (takeback/draw offer sent, or a safe error).
    OnlineNotice {
        message: String,
    },
    /// `config.toml` changed on disk (watcher thread); reload and repaint.
    ConfigChanged,
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
    /// Initial position as sent by `gameFull` (`"startpos"` or a FEN); the
    /// authoritative board is always rebuilt from this plus the whole move
    /// list, so a missed frame cannot leave a stale position behind.
    initial_fen: String,
    /// What the player sees: the server board plus, while a move is in flight,
    /// the optimistic local move.
    board: VariantPosition,
    /// The last position the server confirmed.
    server_board: VariantPosition,
    /// UCI tokens from the last stream frame.
    server_moves: Vec<String>,
    /// Our move that has not been confirmed yet.
    pending: Option<PendingMove>,
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

/// A move applied optimistically: its UCI spelling and the move-list index it
/// must occupy once the server echoes it.
struct PendingMove {
    uci: String,
    index: usize,
}

enum OnlineState {
    /// No online flow running.
    Idle,
    /// The token is being validated and the challenge created.
    Challenging,
    /// The challenge exists; waiting for the opponent to accept it.
    WaitingAccept { challenge_id: String },
    /// A running Board API game. Only this state accepts local moves.
    Active(Box<OnlineGame>),
    /// The game ended (resign, mate, clock, draw); the final board stays up.
    Finished(Box<OnlineGame>),
    /// The flow or stream failed. A game, when present, stays visible but is
    /// no longer playable.
    Failed {
        message: String,
        game: Option<Box<OnlineGame>>,
    },
}

impl OnlineState {
    /// The game to display, in any state that has one.
    fn game(&self) -> Option<&OnlineGame> {
        match self {
            OnlineState::Active(game)
            | OnlineState::Finished(game)
            | OnlineState::Failed {
                game: Some(game), ..
            } => Some(game),
            _ => None,
        }
    }

    /// The game local moves may be applied to.
    fn active_mut(&mut self) -> Option<&mut OnlineGame> {
        match self {
            OnlineState::Active(game) => Some(game),
            _ => None,
        }
    }

    fn is_startable(&self) -> bool {
        matches!(
            self,
            OnlineState::Idle | OnlineState::Finished(_) | OnlineState::Failed { .. }
        )
    }
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
    token: Option<lichess::Token>,
    api: lichess::ApiClient,
    my_username: Option<String>,
    online: OnlineState,
    /// Text in the challenge panel's opponent field.
    challenge_opponent: String,
    /// Validation/token error shown inside the open challenge panel.
    challenge_error: Option<String>,
    /// Transient result of a takeback/draw/resign request.
    online_notice: Option<String>,
    /// SAN of every move seen in the current game, oldest first. Rebuilt only
    /// from applied/derived moves; cleared on a new game or a mode switch.
    history: Vec<String>,
    /// Transient Atomic blast; `None` when idle and in every other variant.
    explosion: Option<Explosion>,
    /// Active theme selection (board palette + SVG piece set).
    config: config::Config,
    /// Where the loaded config came from; `None` in tests and when neither
    /// XDG_CONFIG_HOME nor HOME is set (hot-reload is then unavailable).
    config_path: Option<PathBuf>,
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

/// `3+2` style label for a `(limit, increment)` time control in seconds.
fn clock_label((limit, increment): (u32, u32)) -> String {
    match (limit % 60, limit / 60) {
        (0, minutes) => format!("{minutes}+{increment}"),
        _ => format!("{limit}s+{increment}"),
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

#[cfg(test)]
fn derive_last_move(prev: &VariantPosition, next: &VariantPosition) -> Option<(Square, Square)> {
    let mv = derive_played_move(prev, next)?;
    // Variant-only moves (Crazyhouse drops) have no origin square.
    Some((mv.from()?, move_destination(mv, prev)))
}

/// Atomic blast flash lifetime, in milliseconds.
const EXPLOSION_MS: f32 = 500.0;
/// Captured pieces disappear faster than the rest of the blast.
const CAPTURED_FADE_MS: f32 = 200.0;
/// Peak flash alpha over the base square colour (the UI blends it in).
const EXPLOSION_ALPHA: f32 = 0.8;

/// One piece the blast removed, snapshotted from the pre-move position so it
/// can fade out over the after-move board.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ExplosionGhost {
    role: Role,
    color: Color,
    square: Square,
    /// The piece captured on the destination square (fades in 200 ms).
    captured: bool,
}

/// A transient Atomic explosion. The board already holds the result; this is
/// purely the visual: the flashed radius, the removed pieces, and when the
/// effect started.
struct Explosion {
    squares: Vec<Square>,
    ghosts: Vec<ExplosionGhost>,
    start: Instant,
}

/// Blast radius in Atomic: the capture square plus its Moore neighbourhood,
/// clipped to the board. shakmaty removes the same neighbourhood (minus
/// explosion-immune pawns); this list is only for the flash.
fn explosion_squares(center: Square) -> Vec<Square> {
    let file = center.file().to_u32() as i32;
    let rank = center.rank().to_u32() as i32;
    let mut squares = Vec::with_capacity(9);
    for df in -1..=1 {
        for dr in -1..=1 {
            let f = file + df;
            let r = rank + dr;
            if (0..8).contains(&f) && (0..8).contains(&r) {
                squares.push(Square::new((r * 8 + f) as u32));
            }
        }
    }
    squares.sort_by_key(|sq| *sq as u8);
    squares
}

/// The square whose piece was captured: the destination, except for en
/// passant where the pawn stands beside it.
fn captured_square(mv: Move) -> Option<Square> {
    match mv {
        Move::Normal { capture: Some(_), to, .. } => Some(to),
        Move::EnPassant { from, to } => Some(Square::from_coords(to.file(), from.rank())),
        _ => None,
    }
}

/// Build the visual blast for `mv` when it is a capture in an Atomic position.
/// `prev` is the position before the move and `next` after it. Ghosts are only
/// pieces shakmaty actually removed, so explosion-immune pawns stay solid.
fn explosion_for(
    prev: &VariantPosition,
    next: &VariantPosition,
    mv: Move,
) -> Option<Explosion> {
    if prev.variant() != Variant::Atomic {
        return None;
    }
    let captured = captured_square(mv)?;
    let squares = explosion_squares(mv.to());
    let ghosts = squares
        .iter()
        .filter(|sq| Some(**sq) != mv.from())
        .filter_map(|&sq| {
            let before = prev.board().piece_at(sq)?;
            // The mover left its origin; pawns are immune; anything else that
            // changed on these squares was destroyed by the blast.
            if next.board().piece_at(sq) == Some(before) {
                return None;
            }
            Some(ExplosionGhost {
                role: before.role,
                color: before.color,
                square: sq,
                captured: sq == captured,
            })
        })
        .collect();
    Some(Explosion {
        squares,
        ghosts,
        start: Instant::now(),
    })
}

/// Flash strength at `elapsed_ms`: bright at the start, easing to zero across
/// the 500 ms window.
fn explosion_flash(elapsed_ms: f32) -> f32 {
    let t = (elapsed_ms / EXPLOSION_MS).clamp(0.0, 1.0);
    EXPLOSION_ALPHA * (1.0 - t * t)
}

/// Ghost alpha: linear 1 → 0 over the blast window; the captured piece uses a
/// shorter 200 ms window so the capture reads immediately.
fn explosion_piece_alpha(elapsed_ms: f32, captured: bool) -> f32 {
    let window = if captured {
        CAPTURED_FADE_MS
    } else {
        EXPLOSION_MS
    };
    (1.0 - elapsed_ms / window).clamp(0.0, 1.0)
}

/// Screen centre of a board square for the extra-piece pass.
fn piece_center(offset: (f32, f32), sq: f32, square: Square) -> (f32, f32) {
    (
        offset.0 + (square.file().to_u32() as f32 + 0.5) * sq,
        offset.1 + (7.0 - square.rank().to_u32() as f32 + 0.5) * sq,
    )
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

/// Grouped Crazyhouse pocket contents in tray order (pawns first), straight
/// from shakmaty's authoritative pockets. Every other variant has no pockets,
/// so the pocket UI simply stays empty there.
fn pocket_items(pos: &VariantPosition, color: Color) -> Vec<(Role, u8)> {
    let Some(pockets) = pos.pockets() else {
        return Vec::new();
    };
    let by_role = pockets.get(color);
    [Role::Pawn, Role::Knight, Role::Bishop, Role::Rook, Role::Queen]
        .into_iter()
        .filter_map(|role| {
            let count = *by_role.get(role);
            (count > 0).then_some((role, count))
        })
        .collect()
}

/// Empty squares where `role` may be dropped, taken from shakmaty's legal
/// move list. Drop legality is never decided by the UI.
fn drop_squares(pos: &VariantPosition, role: Role) -> Vec<Square> {
    pos.legal_moves()
        .into_iter()
        .filter_map(|mv| match mv {
            Move::Put { role: dropped, to } if dropped == role => Some(to),
            _ => None,
        })
        .collect()
}

/// The legal drop of `role` onto `to`, if one exists.
fn find_drop(pos: &VariantPosition, role: Role, to: Square) -> Option<Move> {
    pos.legal_moves().into_iter().find(|&mv| {
        matches!(
            mv,
            Move::Put { role: dropped, to: square } if dropped == role && square == to
        )
    })
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
/// so they always contrast — the same trick Lichess and Chess.com use. The
/// tints follow the active [`ui::BoardTheme`], so every palette stays legible.
fn coord_color(theme: ui::BoardTheme, file: u8, rank: u8) -> [f32; 4] {
    let (light, dark) = theme.colors();
    // a1 is dark; a square is light when file + rank is odd.
    let c = if (file as u32 + rank as u32) % 2 == 1 {
        dark
    } else {
        light
    };
    [c[0], c[1], c[2], 1.0]
}

impl App {
    /// Open the Stage 2 challenge panel instead of starting the flow
    /// immediately. The token file is created (0600) here so the user has a
    /// place to paste it; a missing token is shown inside the panel, not as a
    /// failed game, because the panel is still open for correction.
    fn open_challenge_panel(&mut self) {
        if !self.online.is_startable() {
            return;
        }
        // The auth flow owns the token file: create an empty owner-only file if
        // it is missing, so the user has a 0600 place to paste the token.
        // Existing files are never rewritten or chmod'ed, only checked. Unit
        // tests have no event loop (`proxy` is `None`) and never touch the
        // user's config directory.
        if self.proxy.is_some() {
            if let Some(path) = config::token_path() {
                match permissions::ensure_private_empty_file(&path) {
                    Ok(true) => log::info!("Created {} for the API token", path.display()),
                    Ok(false) => {}
                    Err(e) => log::warn!("Could not create {}: {e}", path.display()),
                }
                permissions::warn_if_shared_readable(&path, "API token");
            }
        }
        self.mode = Mode::Play;
        self.ui.clear_selection();
        self.ui.clear_promotion();
        self.ui.set_last_move(None);
        self.history.clear();
        self.clear_explosion();
        self.online_notice = None;
        self.challenge_error = if lichess::read_token().is_some() {
            None
        } else {
            Some("No API token — set it in ~/.config/lichess-rs/token".to_string())
        };
        if self.challenge_opponent.is_empty() {
            if let Some(name) = read_opponent() {
                self.challenge_opponent = name;
            }
        }
        self.ui.open_challenge();
    }

    /// Build a validated challenge from the panel's current selections. Split
    /// from submission so the validation is testable without a worker.
    fn panel_challenge_request(&self) -> Result<lichess::ChallengeRequest, String> {
        let opponent = self.challenge_opponent.trim();
        lichess::validate_username(opponent).map_err(|e| e.to_string())?;
        let (limit, increment) = self.ui.challenge_clock();
        let color = match self.ui.challenge_color_index() {
            0 => lichess::ChallengeColor::White,
            1 => lichess::ChallengeColor::Black,
            _ => lichess::ChallengeColor::Random,
        };
        lichess::ChallengeRequest::new(opponent, false, limit, increment, color)
            .map_err(|e| e.to_string())
    }

    /// Validate the panel, read the token, close the panel and hand the
    /// long-running work (challenge keep-alive + game stream) to a worker.
    /// The state machine is `Idle → Challenging`, then the worker reports
    /// `WaitingAccept` / `Active` / `Failed`.
    fn submit_challenge(&mut self) {
        if !self.online.is_startable() {
            return;
        }
        let request = match self.panel_challenge_request() {
            Ok(request) => request,
            Err(message) => {
                self.challenge_error = Some(message);
                return;
            }
        };
        // A test can seed `self.token`; production always reads the file/env.
        let Some(token) = self.token.clone().or_else(lichess::read_token) else {
            self.challenge_error =
                Some("No API token — set it in ~/.config/lichess-rs/token".to_string());
            return;
        };
        self.token = Some(token.clone());
        self.challenge_error = None;
        self.online_notice = None;
        self.ui.close_challenge();
        self.ui.clear_selection();
        self.online = OnlineState::Challenging;
        let Some(proxy) = self.proxy.clone() else {
            return;
        };
        let api = self.api.clone();
        std::thread::spawn(move || online_worker(proxy, api, token, request));
    }

    /// Keyboard input while the challenge panel is open. Only the opponent
    /// field edits text; Enter submits, Escape closes. Returns whether the
    /// frame needs repainting.
    fn handle_challenge_key(&mut self, code: KeyCode, text: Option<&str>) -> bool {
        match code {
            KeyCode::Escape => {
                self.ui.close_challenge();
                true
            }
            KeyCode::Enter | KeyCode::NumpadEnter => {
                self.submit_challenge();
                true
            }
            KeyCode::Backspace => {
                self.challenge_opponent.pop();
                true
            }
            _ => {
                let Some(text) = text else {
                    return false;
                };
                let mut changed = false;
                for c in text.chars() {
                    if self.challenge_opponent.chars().count() >= 30 {
                        break;
                    }
                    if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                        self.challenge_opponent.push(c);
                        changed = true;
                    }
                }
                changed
            }
        }
    }

    /// Mouse input while the challenge panel is open. A click outside the
    /// panel (or Cancel) closes it; the typed opponent is kept for next time.
    fn handle_challenge_click(&mut self, x: f32, y: f32) {
        match self.ui.challenge_hit(x, y) {
            Some(ui::ChallengeHit::Clock(index)) => self.ui.select_challenge_clock(index),
            Some(ui::ChallengeHit::Color(index)) => self.ui.select_challenge_color(index),
            Some(ui::ChallengeHit::Create) => self.submit_challenge(),
            Some(ui::ChallengeHit::Cancel) | None => self.ui.close_challenge(),
            Some(ui::ChallengeHit::Opponent) | Some(ui::ChallengeHit::Inside) => {}
        }
    }

    /// `OnlineChallengeCreated`: the challenge exists, so only the opponent's
    /// acceptance is missing. Ignored unless we were still validating; a late
    /// event must not pull an active game back to `WaitingAccept`.
    fn on_online_challenge_created(&mut self, challenge_id: String) {
        if matches!(self.online, OnlineState::Challenging) {
            log::info!("Challenge {challenge_id} sent; waiting for acceptance");
            self.online = OnlineState::WaitingAccept { challenge_id };
        }
    }

    /// `gameFull`: set up the running game and enter `Active` (or `Finished`
    /// when the first frame already carries a terminal status).
    fn on_online_full(&mut self, full: lichess::GameFull) {
        let Some(me) = self.my_username.clone() else {
            self.on_online_failed("Online account was not loaded".to_string());
            return;
        };
        let my_color = if full.white.name.eq_ignore_ascii_case(&me) {
            Color::White
        } else {
            Color::Black
        };
        let initial_fen = full.initial_fen.unwrap_or_else(|| "startpos".to_string());
        let server_moves = split_moves(&full.state.moves);
        let server_board = game_position(&initial_fen, &server_moves);
        // A fresh online game starts without a last-move highlight or notices.
        self.ui.set_last_move(None);
        self.history.clear();
        self.online_notice = None;
        self.challenge_error = None;
        let game_over = full.state.status != "started";
        let game = Box::new(OnlineGame {
            game_id: full.id,
            initial_fen,
            board: server_board.clone(),
            server_board,
            server_moves,
            pending: None,
            my_color,
            white_name: full.white.name,
            black_name: full.black.name,
            white_rating: full.white.rating.unwrap_or(0),
            black_rating: full.black.rating.unwrap_or(0),
            white_clock: full.state.wtime as f64 / 1000.0,
            black_clock: full.state.btime as f64 / 1000.0,
            last_update: Instant::now(),
            game_over,
            winner: winner_color(full.state.winner.as_deref()),
        });
        self.online = if game_over {
            OnlineState::Finished(game)
        } else {
            OnlineState::Active(game)
        };
    }

    /// `gameState`: reconcile the optimistic board with the authoritative move
    /// list. Our move is confirmed when the server echoes the same UCI token
    /// at the index it was played; if the server skipped it (a rejection) the
    /// board rolls back to the last server position. Unconfirmed states keep
    /// the optimistic board on screen, so play never waits for a round trip.
    fn on_online_state(&mut self, state: lichess::GameState) {
        let game_over = state.status != "started";
        let winner = winner_color(state.winner.as_deref());
        let new_moves = split_moves(&state.moves);
        let mut tail_history = Vec::new();
        let mut tail_last = None;
        let mut tail_explosion = None;
        let mut rollback_optimistic = false;
        let mut rejected = false;
        let mut accepted = false;

        if let OnlineState::Active(game) = &mut self.online {
            let mut tail_start = game.server_moves.len().min(new_moves.len());
            if let Some(pending) = game.pending.take() {
                if new_moves.get(pending.index) == Some(&pending.uci) {
                    // Confirmed: the optimistic SAN is already in history.
                    accepted = true;
                    tail_start = (pending.index + 1).min(new_moves.len());
                } else if new_moves.len() <= pending.index {
                    // The stream has not caught up yet: keep showing the move.
                    game.pending = Some(pending);
                    tail_start = new_moves.len();
                } else {
                    // The server moved past our index with a different move.
                    rejected = true;
                    rollback_optimistic = true;
                    tail_start = pending.index.min(new_moves.len());
                }
            }

            // Replay only the unseen tail on the old authoritative board, so
            // every new ply contributes exactly one SAN entry and the final
            // one drives the last-move highlight and Atomic blast. On
            // confirmation the optimistic board already contains our move,
            // so the tail starts after it.
            let mut cursor = if accepted {
                game.board.clone()
            } else {
                game.server_board.clone()
            };
            for uci in &new_moves[tail_start..] {
                let Ok(parsed) = UciMove::from_ascii(uci.as_bytes()) else {
                    log::warn!("UCI replay stopped at {uci}");
                    break;
                };
                let Ok(mv) = parsed.to_move(&cursor) else {
                    log::warn!("UCI replay stopped at {uci}");
                    break;
                };
                tail_history.push(San::from_move(&cursor, mv).to_string());
                let prev = cursor.clone();
                cursor.play_unchecked(mv);
                tail_last = mv.from().map(|from| (from, move_destination(mv, &prev)));
                tail_explosion = explosion_for(&prev, &cursor, mv);
            }

            game.server_moves = new_moves;
            game.server_board = game_position(&game.initial_fen, &game.server_moves);
            // An unconfirmed pending move keeps `board` optimistic; otherwise
            // the authoritative position wins (including the rollback case).
            if game.pending.is_none() {
                game.board = game.server_board.clone();
            }
            game.white_clock = state.wtime as f64 / 1000.0;
            game.black_clock = state.btime as f64 / 1000.0;
            game.last_update = Instant::now();
            game.game_over = game_over;
            game.winner = winner;
        }

        if rollback_optimistic {
            self.history.pop();
        }
        self.history.extend(tail_history);
        if rejected {
            self.online_notice = Some("Move rejected — board restored".to_string());
            log::warn!("Online move was not echoed by the server; rolled back");
        }
        if let Some(last) = tail_last {
            self.ui.set_last_move(Some(last));
            // Background streams must not animate another mode's game.
            if self.mode == Mode::Play {
                self.explosion = tail_explosion;
            }
        }
        if game_over {
            let previous = std::mem::replace(&mut self.online, OnlineState::Idle);
            if let OnlineState::Active(game) = previous {
                self.online = OnlineState::Finished(game);
            } else {
                self.online = previous;
            }
        }
    }

    /// The HTTP layer could not deliver a move (or Lichess refused it). Roll
    /// the optimistic board back immediately; if the stream later turns out to
    /// have accepted the move anyway, normal reconciliation re-advances and
    /// re-adds the SAN exactly once.
    fn on_online_move_rejected(&mut self, message: String) {
        let message = match &self.token {
            Some(token) => lichess::redact(&message, token.as_str()),
            None => message,
        };
        let mut rolled_back = false;
        if let OnlineState::Active(game) = &mut self.online {
            if game.pending.take().is_some() {
                game.board = game.server_board.clone();
                rolled_back = true;
            }
        }
        if rolled_back {
            self.history.pop();
            // The highlight referred to the optimistic move; nothing on the
            // restored board produced it.
            self.ui.set_last_move(None);
            if self.mode == Mode::Play {
                self.explosion = None;
            }
        }
        log::warn!("{message}");
        self.online_notice = Some(message);
    }

    /// Any worker error lands here. The last board stays visible when one
    /// exists, but the state is no longer playable.
    fn on_online_failed(&mut self, message: String) {
        let previous = std::mem::replace(&mut self.online, OnlineState::Idle);
        if let OnlineState::WaitingAccept { challenge_id } = &previous {
            log::warn!("Challenge {challenge_id} did not start: {message}");
        }
        let game = match previous {
            OnlineState::Active(game) | OnlineState::Finished(game) => Some(game),
            OnlineState::Failed { game, .. } => game,
            _ => None,
        };
        // Belt and braces: no code path may echo the token into the banner.
        let message = match &self.token {
            Some(token) => lichess::redact(&message, token.as_str()),
            None => message,
        };
        log::warn!("Online play failed: {message}");
        self.online_notice = None;
        self.online = OnlineState::Failed { message, game };
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
        // The challenge panel is modal: drawing the board's coordinates,
        // bars or history through it would look broken, so it owns the whole
        // text pass while open.
        if self.ui.challenge_open() {
            return self.challenge_texts();
        }
        let mut texts = Vec::new();
        self.push_coordinates(&mut texts);
        self.push_player_bars(&mut texts);
        self.push_pocket_badges(&mut texts);
        self.push_move_history(&mut texts);
        self.push_control_texts(&mut texts);
        texts
    }

    /// Text for the challenge dialog, positioned from `ui`'s geometry. The
    /// panel quads draw over the board; this text draws over the quads.
    fn challenge_texts(&self) -> Vec<renderer::TextElement> {
        let mut texts = Vec::new();
        let Some(panel) = self.ui.challenge_panel() else {
            return texts;
        };
        let white = [0.96, 0.96, 0.96, 1.0];
        let muted = [0.66, 0.66, 0.69, 1.0];
        let error = [0.97, 0.55, 0.48, 1.0];
        let base = (panel.panel.w * 0.036).clamp(12.0, 17.0);
        let title_size = (panel.panel.w * 0.055).clamp(16.0, 24.0);
        let centered = |rect: &ui::Rect| rect.y + rect.h * 0.5 + base * 0.35;
        let left = panel.opponent.x;

        texts.push(renderer::TextElement {
            text: "New challenge".to_string(),
            x: left,
            y: panel.opponent.y - title_size * 0.9,
            size: title_size,
            color: white,
            align: renderer::TextAlign::Left,
            shadow: false,
        });

        let opponent = self.challenge_opponent.trim();
        texts.push(renderer::TextElement {
            text: if opponent.is_empty() {
                "Opponent username".to_string()
            } else {
                opponent.to_string()
            },
            x: left + panel.opponent.h * 0.35,
            y: centered(&panel.opponent),
            size: base,
            color: if opponent.is_empty() { muted } else { white },
            align: renderer::TextAlign::Left,
            shadow: false,
        });
        // Field label, drawn small above the input.
        texts.push(renderer::TextElement {
            text: "Opponent".to_string(),
            x: left,
            y: panel.opponent.y - base * 0.45,
            size: base * 0.8,
            color: muted,
            align: renderer::TextAlign::Left,
            shadow: false,
        });

        for (i, rect) in panel.clocks.iter().enumerate() {
            texts.push(renderer::TextElement {
                text: clock_label(ui::CHALLENGE_CLOCKS[i]),
                x: rect.x + rect.w * 0.5,
                y: centered(rect),
                size: base,
                color: white,
                align: renderer::TextAlign::Center,
                shadow: false,
            });
        }
        for (i, rect) in panel.colors.iter().enumerate() {
            texts.push(renderer::TextElement {
                text: ui::CHALLENGE_COLORS[i].to_string(),
                x: rect.x + rect.w * 0.5,
                y: centered(rect),
                size: base,
                color: white,
                align: renderer::TextAlign::Center,
                shadow: false,
            });
        }
        for (rect, label) in [(panel.cancel, "Cancel"), (panel.create, "Create")] {
            texts.push(renderer::TextElement {
                text: label.to_string(),
                x: rect.x + rect.w * 0.5,
                y: centered(&rect),
                size: base,
                color: white,
                align: renderer::TextAlign::Center,
                shadow: false,
            });
        }

        let (status, color) = match &self.challenge_error {
            Some(message) => (message.as_str(), error),
            None => ("Unrated · Enter to send · Esc to cancel", muted),
        };
        texts.push(renderer::TextElement {
            text: status.to_string(),
            x: panel.status.x,
            y: centered(&panel.status),
            size: base * 0.85,
            color,
            align: renderer::TextAlign::Left,
            shadow: false,
        });
        texts
    }

    /// Labels for the Takeback / Draw / Resign row and the last action notice.
    fn push_control_texts(&self, out: &mut Vec<renderer::TextElement>) {
        if self.mode != Mode::Play {
            return;
        }
        let controls = self.ui.controls();
        for button in controls {
            let label = match button.kind {
                ui::ControlKind::Takeback => "Takeback",
                ui::ControlKind::Draw => "Draw",
                ui::ControlKind::Resign => "Resign",
            };
            let size = (button.rect.h * 0.42).clamp(10.0, 16.0);
            out.push(renderer::TextElement {
                text: label.to_string(),
                x: button.rect.x + button.rect.w * 0.5,
                y: button.rect.y + button.rect.h * 0.5 + size * 0.35,
                size,
                color: [0.96, 0.96, 0.96, 1.0],
                align: renderer::TextAlign::Center,
                shadow: false,
            });
        }
        if let (Some(notice), Some(first)) = (&self.online_notice, controls.first()) {
            let size = (first.rect.h * 0.36).clamp(10.0, 14.0);
            out.push(renderer::TextElement {
                text: notice.clone(),
                x: self.ui.board_offset().0 + self.ui.square_size() * 8.0,
                y: first.rect.y + first.rect.h * 0.5 + size * 0.35,
                size,
                color: [0.85, 0.85, 0.82, 1.0],
                align: renderer::TextAlign::Right,
                shadow: false,
            });
        }
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
                    alpha: 1.0,
                });
                x += icon + gap;
            }
        }
    }

    /// Pocket icons for both rows; usable pieces are solid, display-only ones
    /// are dimmed. They use the same `Scene.pieces` path as the promotion menu.
    fn push_pocket_icons(&self, out: &mut Vec<renderer::PieceElement>) {
        for color in [Color::Black, Color::White] {
            for p in self.ui.pockets(color) {
                out.push(renderer::PieceElement {
                    role: p.role,
                    color: p.color,
                    x: p.rect.x + p.rect.w * 0.5,
                    y: p.rect.y + p.rect.h * 0.5,
                    size: p.rect.w,
                    alpha: p.alpha(),
                });
            }
        }
    }

    /// `x2` count badges beside grouped pocket pieces, plus an ellipsis when a
    /// row had to be truncated at the board edge.
    fn push_pocket_badges(&self, texts: &mut Vec<renderer::TextElement>) {
        let sq = self.ui.square_size();
        if sq <= 0.0 {
            return;
        }
        let size = (sq * 0.22).max(10.0);
        for color in [Color::Black, Color::White] {
            for p in self.ui.pockets(color) {
                let Some(badge) = p.badge() else {
                    continue;
                };
                texts.push(renderer::TextElement {
                    text: badge,
                    x: p.rect.x + p.rect.w + sq * 0.01,
                    y: p.rect.y + p.rect.h * 0.5 + size * 0.35,
                    size,
                    color: [0.92, 0.92, 0.92, if p.interactive { 1.0 } else { 0.5 }],
                    align: renderer::TextAlign::Left,
                    shadow: false,
                });
            }
            if self.ui.pocket_truncated(color) {
                if let Some(last) = self.ui.pockets(color).last() {
                    texts.push(renderer::TextElement {
                        text: "\u{2026}".to_string(),
                        x: self.ui.pocket_end(color),
                        y: last.rect.y + last.rect.h * 0.5 + size * 0.35,
                        size,
                        color: [0.92, 0.92, 0.92, 1.0],
                        align: renderer::TextAlign::Left,
                        shadow: false,
                    });
                }
            }
        }
    }

    /// Recompute the displayed pocket rows from the current position. Run
    /// before layout/render and before hit-testing; it is a cheap, idempotent
    /// derivation, never a second source of truth for pocket contents.
    fn sync_pockets(&mut self) {
        let (black, white, black_usable, white_usable) = {
            let board = match self.mode {
                Mode::Live => self.live_board.as_ref().unwrap_or(&self.board),
                Mode::Local => &self.board,
                Mode::Play => self.online_game().map(|game| &game.board).unwrap_or(&self.board),
            };
            let (black_usable, white_usable) = match self.mode {
                Mode::Live => (false, false),
                Mode::Local => {
                    let live = !self.local_game_over();
                    (
                        live && board.turn() == Color::Black,
                        live && board.turn() == Color::White,
                    )
                }
                Mode::Play => {
                    match &self.online {
                        OnlineState::Active(game)
                            if !game.game_over && game.board.turn() == game.my_color =>
                        {
                            (
                                game.my_color == Color::Black,
                                game.my_color == Color::White,
                            )
                        }
                        _ => (false, false),
                    }
                }
            };
            (
                pocket_items(board, Color::Black),
                pocket_items(board, Color::White),
                black_usable,
                white_usable,
            )
        };
        self.ui.set_pocket(Color::Black, &black, black_usable);
        self.ui.set_pocket(Color::White, &white, white_usable);
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
                OnlineState::Active(game) if self.mode == Mode::Play && !game.game_over => {
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
                color: coord_color(self.config.board, file, 0),
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
                color: coord_color(self.config.board, 7, rank),
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
            Mode::Play => self.online.active_mut().map(|game| &mut game.board),
            Mode::Live => None,
        }
    }

    /// Post a played move to Lichess in the background. The board already
    /// advanced optimistically; the stream confirms (or a failure rolls back).
    fn post_online_move(&mut self, mv: Move) {
        let game_id = self.online_game().map(|game| game.game_id.clone());
        let Some(game_id) = game_id else { return };
        let Some(token) = self.token.clone() else { return };
        // Unit tests have no event loop and must never hit the network.
        let Some(proxy) = self.proxy.clone() else {
            return;
        };
        let api = self.api.clone();
        let uci = mv.to_uci(CastlingMode::Standard).to_string();
        std::thread::spawn(move || {
            if let Err(e) = api.play_board_move(&token, &game_id, &uci) {
                let message = online_error("Move failed", &e, &token);
                log::warn!("{message}");
                let _ = proxy.send_event(NetEvent::OnlineMoveRejected { message });
            }
        });
    }

    /// The online game to display, in any state that still has a board.
    fn online_game(&self) -> Option<&OnlineGame> {
        self.online.game()
    }

    /// Show the Takeback / Draw / Resign row only while a Board API game is
    /// running and movable; a finished or failed game keeps the final board
    /// but has nothing to send.
    fn sync_controls(&mut self) {
        let visible = self.mode == Mode::Play
            && matches!(&self.online, OnlineState::Active(game) if !game.game_over);
        self.ui.set_controls_visible(visible);
    }

    /// Send a takeback/draw offer or a resignation. The notice is optimistic
    /// ("… sent"); the game stream reports the real outcome, and a transport
    /// failure replaces the notice with a redacted message.
    fn send_control(&mut self, kind: ui::ControlKind) {
        let Some(game) = self.online_game() else { return };
        if !matches!(self.online, OnlineState::Active(_)) {
            return;
        }
        let game_id = game.game_id.clone();
        let Some(token) = self.token.clone() else {
            self.online_notice = Some("No API token — cannot send that".to_string());
            return;
        };
        let api = self.api.clone();
        self.online_notice = Some(
            match kind {
                ui::ControlKind::Takeback => "Takeback offer sent",
                ui::ControlKind::Draw => "Draw offer sent",
                ui::ControlKind::Resign => "Resigning…",
            }
            .to_string(),
        );
        // Unit tests have no event loop; the optimistic notice above is the
        // observable part there, and no request leaves the process.
        let Some(proxy) = self.proxy.clone() else {
            return;
        };
        std::thread::spawn(move || {
            let result = match kind {
                ui::ControlKind::Takeback => api.takeback(&token, &game_id),
                ui::ControlKind::Draw => api.offer_draw(&token, &game_id),
                ui::ControlKind::Resign => api.resign(&token, &game_id),
            };
            if let Err(e) = result {
                let message = online_error("Online control failed", &e, &token);
                log::warn!("{message}");
                let _ = proxy.send_event(NetEvent::OnlineNotice { message });
            }
        });
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
        // Atomic captures need the pre-move position to snapshot the pieces
        // the blast removes. The board is updated immediately either way.
        let atomic_prev = if self.mode == Mode::Local {
            match self.active_board_ref() {
                Some(pos) if pos.variant() == Variant::Atomic && mv.capture().is_some() => {
                    Some(pos.clone())
                }
                _ => None,
            }
        } else {
            None
        };
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
            self.explosion =
                atomic_prev.and_then(|prev| explosion_for(&prev, &self.board, mv));
        } else {
            // Without a token there is nothing to post; leave the board and
            // history untouched rather than showing a move that never left.
            if self.token.is_none() {
                self.online_notice = Some("No API token — cannot play that move".to_string());
                return;
            }
            // Optimistic apply: the board advances now and the move is marked
            // pending until the server echoes it (or a rollback arrives).
            if let Some(game) = self.online.active_mut() {
                let uci = mv.to_uci(CastlingMode::Standard).to_string();
                let index = game.server_moves.len();
                game.board.play_unchecked(mv);
                game.pending = Some(PendingMove { uci, index });
            }
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
                // Background TV keeps running in every mode; only the Live
                // view may turn its captures into a blast.
                if self.mode == Mode::Live {
                    self.explosion = explosion_for(prev, board, mv);
                }
            }
            None => {
                self.ui.set_last_move(None);
                if self.mode == Mode::Live {
                    self.explosion = None;
                }
            }
        }
    }

    /// Age the current blast by one frame: drop it when the window is over,
    /// otherwise push the eased flash strength into the UI. Idle frames (no
    /// explosion) clear a leftover tint once and cost nothing afterwards.
    fn tick_explosion(&mut self) -> Option<f32> {
        let elapsed = self
            .explosion
            .as_ref()
            .map(|explosion| explosion.start.elapsed().as_secs_f32() * 1000.0);
        let Some(elapsed) = elapsed else {
            self.ui.clear_explosion();
            return None;
        };
        if elapsed >= EXPLOSION_MS {
            self.explosion = None;
            self.ui.clear_explosion();
            return None;
        }
        if let Some(explosion) = &self.explosion {
            self.ui
                .set_explosion(&explosion.squares, explosion_flash(elapsed));
        }
        Some(elapsed)
    }

    fn clear_explosion(&mut self) {
        self.explosion = None;
        self.ui.clear_explosion();
    }

    /// Adopt a freshly loaded config: retint the board, re-rasterize the piece
    /// atlas when the set changed, and remember the selection. Everything is
    /// conditional so a save that only touches one key does the minimum work.
    fn apply_config(&mut self, config: config::Config) {
        if config.board != self.config.board {
            self.ui.set_board_theme(config.board);
        }
        if config.pieces != self.config.pieces {
            if let Some(rend) = self.renderer.as_mut() {
                rend.reload_pieces(config.pieces.name());
            }
        }
        self.config = config;
    }

    fn handle_board_click(&mut self, x: f32, y: f32) {
        if self.mode == Mode::Live {
            return;
        }
        // Pocket geometry follows the displayed board and whose turn it is.
        self.sync_pockets();

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
            Mode::Play => match &self.online {
                OnlineState::Active(game)
                    if !game.game_over && game.board.turn() == game.my_color =>
                {
                    true
                }
                _ => return,
            },
            Mode::Live => unreachable!(),
        };

        // A usable pocket piece starts a drop selection: its legal drops come
        // from shakmaty's move list and use the ordinary target tint.
        if let Some((_, role)) = self.ui.pocket_at(x, y) {
            if my_turn {
                let targets = self
                    .active_board_ref()
                    .map(|board| drop_squares(board, role))
                    .unwrap_or_default();
                self.ui.select_pocket(role, targets);
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
            if let Some(role) = self.ui.selected_pocket_piece() {
                // A drop: find the matching `Move::Put` and play it through
                // the same path as board moves.
                if my_turn {
                    let mv = self
                        .active_board_ref()
                        .and_then(|board| find_drop(board, role, sq));
                    if let Some(mv) = mv {
                        self.apply_move(mv);
                    }
                }
                self.ui.clear_selection();
                return;
            }
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
            Mode::Play => match &self.online {
                OnlineState::Active(game) => game.my_color == color,
                _ => false,
            },
            Mode::Live => false,
        }
    }

    /// Left button pressed: run the ordinary click handling, then remember a
    /// drag when that press selected a piece. A press that already played a
    /// move (clicking a highlighted target) clears the selection and so never
    /// starts a drag.
    fn handle_mouse_press(&mut self, x: f32, y: f32) {
        // The challenge panel is modal: it swallows the press so no piece
        // behind it can be selected or dragged.
        if self.ui.challenge_open() {
            self.handle_challenge_click(x, y);
            return;
        }
        // Action buttons sit in the bottom margin, off the board; a press on
        // one sends the request instead of starting a board interaction.
        if let Some(kind) = self.ui.control_at(x, y) {
            self.send_control(kind);
            return;
        }
        self.handle_board_click(x, y);
        if self.mode == Mode::Live {
            return;
        }
        // Pressing a usable pocket piece arms a drop drag; the click above has
        // already selected it and highlighted its legal landing squares.
        if let Some((_, role)) = self.ui.pocket_at(x, y) {
            if self.ui.selected_pocket_piece() == Some(role) {
                self.ui.start_pocket_drag(role, x, y);
            }
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
        // A panel opened mid-drag (keyboard shortcut) ends the drag without
        // resolving it into a move behind the dialog.
        if self.ui.challenge_open() {
            self.ui.end_drag();
            return false;
        }
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
        self.ui.set_board_theme(self.config.board);
        self.ui.layout(safe_w, safe_h);
        let rend = pollster::block_on(renderer::Renderer::new(
            window.clone(),
            self.config.pieces,
        ));
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
                if self.mode == Mode::Live {
                    self.clear_explosion();
                }
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
            NetEvent::OnlineAccount { username } => {
                self.my_username = Some(username);
            }
            NetEvent::OnlineChallengeCreated { challenge_id } => {
                self.on_online_challenge_created(challenge_id);
            }
            NetEvent::OnlineFull(full) => self.on_online_full(*full),
            NetEvent::OnlineState(state) => self.on_online_state(state),
            NetEvent::OnlineFailed { message } => self.on_online_failed(message),
            NetEvent::OnlineMoveRejected { message } => self.on_online_move_rejected(message),
            NetEvent::OnlineNotice { message } => {
                // Worker messages are already redacted; re-apply locally so a
                // future caller cannot leak the token into the notice line.
                let message = match &self.token {
                    Some(token) => lichess::redact(&message, token.as_str()),
                    None => message,
                };
                log::info!("Online notice: {message}");
                self.online_notice = Some(message);
            }
            NetEvent::ConfigChanged => {
                let Some(path) = self.config_path.clone() else {
                    return;
                };
                // Editors sometimes save through a temp file and reset the
                // mode, so re-check on every reload and never auto-fix it.
                permissions::warn_if_shared_readable(&path, "config file");
                match config::Config::load(&path) {
                    Ok(Some(config)) => {
                        self.apply_config(config);
                        if let Some(window) = self.window.as_ref() {
                            window.request_redraw();
                        }
                    }
                    // A deleted file means "back to the defaults" is equally
                    // defensible, but keeping the running theme is the least
                    // surprising thing to do while the user is editing.
                    Ok(None) => {
                        log::warn!("{} disappeared; keeping the current theme", path.display());
                    }
                    Err(e) => log::warn!("Config reload ignored: {e}"),
                }
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
                needs_redraw = dragging
                    || self.ui.promotion_open()
                    || self.ui.challenge_open()
                    || !self.ui.controls().is_empty();
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
                    text,
                    ..
                },
                ..
            } => {
                // While the challenge panel is open every key belongs to the
                // opponent field (or submits/cancels); global shortcuts such
                // as `L`/`P`/`C` must not fire mid-edit.
                if self.ui.challenge_open() {
                    needs_redraw = self.handle_challenge_key(code, text.as_deref());
                } else {
                    match code {
                        KeyCode::KeyL => {
                            self.mode = Mode::Live;
                            self.ui.clear_selection();
                            self.ui.clear_promotion();
                            self.ui.set_checked_king(None);
                            self.ui.set_last_move(None);
                            self.history.clear();
                            self.clear_explosion();
                            self.online_notice = None;
                            needs_redraw = true;
                        }
                        KeyCode::KeyP => {
                            self.mode = Mode::Local;
                            self.ui.clear_selection();
                            self.refresh_check_highlight();
                            self.ui.set_last_move(None);
                            self.history.clear();
                            self.clear_explosion();
                            self.online_notice = None;
                            needs_redraw = true;
                        }
                        KeyCode::KeyC => {
                            self.open_challenge_panel();
                            needs_redraw = true;
                        }
                        KeyCode::Escape => {
                            self.ui.close_promotion();
                            needs_redraw = true;
                        }
                        _ => {}
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                // Age the Atomic blast first: it also refreshes the square
                // flash before the quads below are built.
                let explosion_age = self.tick_explosion();
                // Derive the pocket rows before anything reads them for layout.
                self.sync_pockets();
                // The action row only exists while an active online game runs.
                self.sync_controls();
                let texts = self.build_texts();
                let cursor = Some((self.cursor_pos.0 as f32, self.cursor_pos.1 as f32));
                let mut quads: Vec<renderer::Quad> = self.ui.squares.iter().map(|r| {
                    renderer::Quad { x: r.x, y: r.y, w: r.w, h: r.h, color: r.color }
                }).collect();

                for r in self.ui.promotion_quads(cursor) {
                    quads.push(renderer::Quad { x: r.x, y: r.y, w: r.w, h: r.h, color: r.color });
                }
                // Action buttons and the modal challenge panel draw after the
                // piece pass, so they cover the board instead of being hidden.
                let mut overlay_quads: Vec<renderer::Quad> = self
                    .ui
                    .control_quads(cursor)
                    .into_iter()
                    .map(|r| renderer::Quad { x: r.x, y: r.y, w: r.w, h: r.h, color: r.color })
                    .collect();
                for r in self.ui.challenge_quads(cursor) {
                    overlay_quads.push(renderer::Quad {
                        x: r.x,
                        y: r.y,
                        w: r.w,
                        h: r.h,
                        color: r.color,
                    });
                }
                // Selection tint behind the chosen pocket piece; quads draw
                // under the piece pass, so it reads as a highlight.
                for r in self.ui.pocket_quads() {
                    quads.push(renderer::Quad { x: r.x, y: r.y, w: r.w, h: r.h, color: r.color });
                }
                let mut pieces: Vec<renderer::PieceElement> = self.ui.promotion_choices().iter().map(|c| {
                    renderer::PieceElement {
                        role: c.role,
                        color: c.color,
                        x: c.rect.x + c.rect.w * 0.5,
                        y: c.rect.y + c.rect.h * 0.5,
                        size: c.rect.h,
                        alpha: 1.0,
                    }
                }).collect();
                // Crazyhouse pocket icons (empty outside Crazyhouse); extra
                // pieces, no new pipeline.
                self.push_pocket_icons(&mut pieces);
                // Captured trays live in the player bars (Live/Play only); they
                // are ordinary extra pieces, no new pipeline.
                if let Some(rend) = self.renderer.as_ref() {
                    self.push_captured_icons(rend, &mut pieces);
                }
                // Removed Atomic pieces fade out over the already-updated
                // board; ghosts are ordinary extra pieces (the capture ghost
                // sits over the capturing piece for its 200 ms fade).
                let (ox, oy) = self.ui.board_offset();
                let sq = self.ui.square_size();
                if let (Some(age), Some(explosion)) = (explosion_age, self.explosion.as_ref()) {
                    for ghost in &explosion.ghosts {
                        let (x, y) = piece_center((ox, oy), sq, ghost.square);
                        pieces.push(renderer::PieceElement {
                            role: ghost.role,
                            color: ghost.color,
                            x,
                            y,
                            size: sq,
                            alpha: explosion_piece_alpha(age, ghost.captured),
                        });
                    }
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
                    Mode::Play => match &self.online {
                        OnlineState::Failed { message, .. } => Some(message.clone()),
                        OnlineState::Challenging => {
                            Some("Validating token and creating the challenge…".to_string())
                        }
                        OnlineState::WaitingAccept { .. } => {
                            Some("Challenge sent - waiting for opponent to accept".to_string())
                        }
                        OnlineState::Finished(game) => {
                            let text = match game.winner {
                                Some(color) if color == game.my_color => "You win!",
                                Some(_) => "You lose",
                                None => "Drawn game",
                            };
                            Some(text.to_string())
                        }
                        _ => None,
                    },
                };
                let banner: Option<&str> = banner.as_deref();

                let board = match self.mode {
                    Mode::Live => self.live_board.as_ref().unwrap_or(&self.board),
                    Mode::Local => &self.board,
                    Mode::Play => match &self.online {
                        OnlineState::Active(game)
                        | OnlineState::Finished(game)
                        | OnlineState::Failed {
                            game: Some(game), ..
                        } => &game.board,
                        _ => &self.board,
                    },
                };

                // Once a press turns into a real drag, lift the piece off its
                // source and let it follow the cursor. A plain click keeps the
                // board untouched (the piece is only hidden from the moment
                // the pointer leaves the click slop). Pocket drags have no
                // board square to hide, but share the lifted piece path.
                let dragged = if self.ui.is_dragging() && self.ui.drag_moved() {
                    let cursor = (self.cursor_pos.0 as f32, self.cursor_pos.1 as f32);
                    let size = self.ui.square_size();
                    if let Some(role) = self.ui.dragging_pocket_piece() {
                        // Only the side to move has a usable pocket, so the
                        // dragged piece takes that side's colour.
                        Some((
                            renderer::DragSource::Pocket,
                            renderer::PieceElement {
                                role,
                                color: board.turn(),
                                x: cursor.0,
                                y: cursor.1,
                                size,
                                alpha: 1.0,
                            },
                        ))
                    } else {
                        self.ui.dragging_piece(board).map(|(from, role, color)| {
                            (
                                renderer::DragSource::Board(from),
                                renderer::PieceElement {
                                    role,
                                    color,
                                    x: cursor.0,
                                    y: cursor.1,
                                    size,
                                    alpha: 1.0,
                                },
                            )
                        })
                    }
                } else {
                    None
                };

                if let Some(rend) = self.renderer.as_mut() {
                    let scene = renderer::Scene {
                        quads: &quads,
                        overlay_quads: &overlay_quads,
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
                // Keep animating only while the blast is alive; once it
                // expires the loop drops back to Wait with no timer.
                needs_redraw = explosion_age.is_some();
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

/// Blocking config watcher: `notify` delivers filesystem events on its own
/// thread; this thread forwards them to the event loop. No polling, no async
/// runtime, and nothing runs again until the next event — idle CPU stays at 0.
fn config_watcher(proxy: EventLoopProxy<NetEvent>, dir: PathBuf) {
    use notify::{RecursiveMode, Watcher};

    let (tx, rx) = std::sync::mpsc::channel();
    // Watch the directory, not the file: editors save by writing a temp file
    // and renaming it over `config.toml`, which would drop a file watch.
    let mut watcher = match notify::recommended_watcher(
        move |result: notify::Result<notify::Event>| {
            if let Ok(event) = result {
                let touches_config = event
                    .paths
                    .iter()
                    .any(|path| path.file_name().is_some_and(|name| name == "config.toml"));
                // Some backends report a directory rename without a path.
                if touches_config || event.paths.is_empty() {
                    let _ = tx.send(());
                }
            }
        },
    ) {
        Ok(watcher) => watcher,
        Err(e) => {
            log::warn!("Config hot-reload unavailable: {e}");
            return;
        }
    };
    if let Err(e) = watcher.watch(&dir, RecursiveMode::NonRecursive) {
        log::warn!("Could not watch {}: {e}", dir.display());
        return;
    }
    log::info!("Watching {} for theme changes", dir.display());

    // One save can emit several events; coalesce the burst into one reload.
    while rx.recv().is_ok() {
        while rx.recv_timeout(Duration::from_millis(120)).is_ok() {}
        let _ = proxy.send_event(NetEvent::ConfigChanged);
    }
    drop(watcher);
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
    let file = config::config_dir()?.join("opponent");
    std::fs::read_to_string(file)
        .ok()
        .and_then(|contents| contents.lines().next().map(|line| line.trim().to_string()))
        .filter(|line| !line.is_empty())
}

/// Replay a full UCI move list (as the Board API stream sends it) onto a
/// board, using the board's own side-to-move tracking.
#[cfg(test)]
fn replay_uci(board: &mut VariantPosition, moves: &str) {
    replay_tokens(board, &split_moves(moves));
}

/// Apply UCI tokens in order, stopping at the first unparsable or illegal one.
fn replay_tokens(board: &mut VariantPosition, moves: &[String]) {
    for uci in moves {
        let Ok(parsed) = UciMove::from_ascii(uci.as_bytes()) else {
            log::warn!("UCI replay stopped at {uci}");
            break;
        };
        match parsed.to_move(board) {
            Ok(mv) => board.play_unchecked(mv),
            Err(_) => {
                log::warn!("UCI replay stopped at {uci}");
                break;
            }
        }
    }
}

/// Split a Board API move list into UCI tokens.
fn split_moves(moves: &str) -> Vec<String> {
    moves.split_whitespace().map(str::to_string).collect()
}

/// Rebuild the authoritative position from the initial FEN and the *whole*
/// move list. Rebuilding beats incrementally patching the board: a duplicated
/// or missed frame can never leave the server board in a state the server did
/// not send.
fn game_position(initial_fen: &str, moves: &[String]) -> VariantPosition {
    let mut board = match initial_fen {
        "startpos" => VariantPosition::default(),
        fen => position_from_fen("standard", fen).unwrap_or_default(),
    };
    replay_tokens(&mut board, moves);
    board
}

/// Turn an upstream Board API error into a short, user-readable message. The
/// full (redacted) detail still goes to the log, so debugging keeps the status
/// and transport context without dumping it into the banner.
fn online_error(context: &str, error: &lichess::Error, token: &lichess::Token) -> String {
    let detail = lichess::redact(&format!("{context}: {error}"), token.as_str());
    log::warn!("{detail}");

    match error {
        lichess::Error::InvalidInput { field, message } => format!("Invalid {field}: {message}"),
        lichess::Error::RateLimited { retry_after } => match retry_after {
            Some(delay) => format!("Lichess rate limit - retry in {} s", delay.as_secs().max(1)),
            None => "Lichess rate limit - retry shortly".to_string(),
        },
        lichess::Error::Parse(_) | lichess::Error::Protocol(_) => {
            "Lichess sent an unexpected response".to_string()
        }
        lichess::Error::Io(_) => "Network error - could not reach Lichess".to_string(),
        lichess::Error::Http(_) => match error.http_status() {
            Some(401) => {
                "Lichess rejected the token - check ~/.config/lichess-rs/token".to_string()
            }
            Some(403) => {
                "Token needs the board:play scope - create a new one on Lichess".to_string()
            }
            Some(404) => format!("{context} - not found on Lichess"),
            Some(429) => "Lichess rate limit - retry shortly".to_string(),
            Some(400) => "Lichess rejected the request".to_string(),
            Some(status) if status >= 500 => "Lichess is having trouble - try again".to_string(),
            Some(status) => format!("Lichess returned HTTP {status}"),
            None => "Network error - could not reach Lichess".to_string(),
        },
    }
}

/// Board API winner strings are `white` / `black`.
fn winner_color(winner: Option<&str>) -> Option<Color> {
    match winner {
        Some(name) if name.eq_ignore_ascii_case("white") => Some(Color::White),
        Some(name) if name.eq_ignore_ascii_case("black") => Some(Color::Black),
        _ => None,
    }
}

/// Long-running online play worker: validate the token, create the
/// challenge, wait for acceptance, then stream the game to the event loop
/// until it ends. Every failure is a safe, token-free message.
fn online_worker(
    proxy: EventLoopProxy<NetEvent>,
    api: lichess::ApiClient,
    token: lichess::Token,
    request: lichess::ChallengeRequest,
) {
    let me = match api.account_me(&token) {
        Ok(me) => me,
        Err(e) => {
            let _ = proxy.send_event(NetEvent::OnlineFailed {
                message: online_error("Token check failed", &e, &token),
            });
            return;
        }
    };
    let _ = proxy.send_event(NetEvent::OnlineAccount {
        username: me.username,
    });

    let opponent = request.username().to_string();
    let mut challenge_id = None;
    let outcome = api.create_challenge(&token, &request, |created| {
        challenge_id = Some(created.id.clone());
        log::info!(
            "Challenge {} sent to {}; keeping it alive until answered",
            created.id,
            opponent
        );
        let _ = proxy.send_event(NetEvent::OnlineChallengeCreated {
            challenge_id: created.id.clone(),
        });
    });
    match outcome {
        Ok(lichess::ChallengeOutcome::Accepted) => {}
        Ok(lichess::ChallengeOutcome::Declined) => {
            let _ = proxy.send_event(NetEvent::OnlineFailed {
                message: format!("{opponent} declined the challenge"),
            });
            return;
        }
        Ok(lichess::ChallengeOutcome::Canceled) => {
            let _ = proxy.send_event(NetEvent::OnlineFailed {
                message: format!("Challenge to {opponent} was canceled or expired"),
            });
            return;
        }
        Err(e) => {
            let _ = proxy.send_event(NetEvent::OnlineFailed {
                message: online_error(&format!("Could not challenge {opponent}"), &e, &token),
            });
            return;
        }
    }

    // The invitation was accepted; the game stream can still 404 for a moment
    // while the game is created, then it stays open until the game ends.
    let Some(challenge_id) = challenge_id else {
        let _ = proxy.send_event(NetEvent::OnlineFailed {
            message: "The challenge was accepted without an id".to_string(),
        });
        return;
    };
    for _ in 0..20 {
        match api.stream_board_game(&token, &challenge_id, |event| {
            forward_board_event(&proxy, event)
        }) {
            Ok(()) => return,
            Err(e) if e.is_not_found() => {
                std::thread::sleep(Duration::from_millis(250));
            }
            Err(e) => {
                let _ = proxy.send_event(NetEvent::OnlineFailed {
                    message: online_error("Game stream failed", &e, &token),
                });
                return;
            }
        }
    }
    let _ = proxy.send_event(NetEvent::OnlineFailed {
        message: "The game did not start after the challenge was accepted".to_string(),
    });
}

/// Map Board API stream events onto the app's event enum.
fn forward_board_event(proxy: &EventLoopProxy<NetEvent>, event: lichess::BoardStreamEvent) {
    let send = match event {
        lichess::BoardStreamEvent::Full(full) => Some(NetEvent::OnlineFull(full)),
        lichess::BoardStreamEvent::State(state) => Some(NetEvent::OnlineState(state)),
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

    // Theme config: the directory is created owner-only (0700), and a missing
    // config is written 0600 from the documented defaults. Both are watched for
    // hot-reload; malformed files fall back to the defaults.
    let config_path = config::config_path();
    let token_path = config::token_path();
    let config = match config_path.as_deref() {
        Some(path) => {
            if let Some(dir) = path.parent() {
                if let Err(e) = permissions::create_private_dir(dir) {
                    log::warn!("Could not create {}: {e}", dir.display());
                }
            }
            match config::ensure_config_file(path) {
                Ok(true) => log::info!("Created default config at {}", path.display()),
                Ok(false) => {}
                Err(e) => log::warn!("{e}"),
            }
            // Never chmod a file the user already had: warn instead.
            permissions::warn_if_shared_readable(path, "config file");
            match config::Config::load(path) {
                Ok(Some(config)) => {
                    log::info!("Theme loaded from {}", path.display());
                    config
                }
                Ok(None) => {
                    log::info!("No {}; using the default theme", path.display());
                    config::Config::default()
                }
                Err(e) => {
                    log::warn!("{e}; using the default theme");
                    config::Config::default()
                }
            }
        }
        None => config::Config::default(),
    };
    if let Some(path) = token_path.as_deref() {
        permissions::warn_if_shared_readable(path, "API token");
    }

    let app_proxy = proxy.clone();
    let watch_proxy = proxy.clone();
    std::thread::spawn(move || lichess_worker(proxy));
    // Blocking watcher thread (same pattern as the TV worker): no polling.
    if let Some(dir) = config_path.as_deref().and_then(|path| path.parent()) {
        let dir = dir.to_path_buf();
        std::thread::spawn(move || config_watcher(watch_proxy, dir));
    }

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
        api: lichess::ApiClient::new(),
        my_username: None,
        online: OnlineState::Idle,
        challenge_opponent: String::new(),
        challenge_error: None,
        online_notice: None,
        history: Vec::new(),
        explosion: None,
        config,
        config_path,
    };
    event_loop.run_app(&mut app).unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_with_board(board: VariantPosition) -> App {
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
            api: lichess::ApiClient::new(),
            my_username: None,
            online: OnlineState::Idle,
            challenge_opponent: String::new(),
            challenge_error: None,
            online_notice: None,
            history: Vec::new(),
            explosion: None,
            config: config::Config::default(),
            config_path: None,
        };
        app.ui.layout(800.0, 600.0);
        app
    }

    fn app_from_fen(fen: &str) -> App {
        app_with_board(position_from_fen("standard", fen).expect("valid test FEN"))
    }

    fn app_from_crazyhouse_fen(fen: &str) -> App {
        app_with_board(position_from_fen("crazyhouse", fen).expect("valid crazyhouse FEN"))
    }

    fn app_from_atomic_fen(fen: &str) -> App {
        app_with_board(position_from_fen("atomic", fen).expect("valid atomic FEN"))
    }

    /// The legal move between two squares, for test setups.
    fn find_move(pos: &VariantPosition, from: Square, to: Square) -> Move {
        pos.legal_moves()
            .into_iter()
            .find(|mv| mv.from() == Some(from) && mv.to() == to)
            .unwrap_or_else(|| panic!("missing legal move {from:?}-{to:?}"))
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

    #[test]
    fn crazyhouse_pockets_come_straight_from_shakmaty() {
        let fen = "4k3/8/8/8/8/8/8/4K3[QRBNPrbn] w - - 0 1";
        let board = position_from_fen("crazyhouse", fen).expect("crazyhouse fen");
        assert_eq!(
            pocket_items(&board, Color::White),
            vec![
                (Role::Pawn, 1),
                (Role::Knight, 1),
                (Role::Bishop, 1),
                (Role::Rook, 1),
                (Role::Queen, 1),
            ]
        );
        assert_eq!(
            pocket_items(&board, Color::Black),
            vec![(Role::Knight, 1), (Role::Bishop, 1), (Role::Rook, 1)]
        );

        let mut app = app_from_crazyhouse_fen(fen);
        app.sync_pockets();
        assert_eq!(app.ui.pockets(Color::White).len(), 5);
        assert_eq!(app.ui.pockets(Color::Black).len(), 3);
        // White is to move, so only White's pocket is usable.
        assert!(app.ui.pockets(Color::White).iter().all(|p| p.interactive));
        assert!(app.ui.pockets(Color::Black).iter().all(|p| !p.interactive));
    }

    #[test]
    fn standard_chess_has_no_pocket_ui_or_drop_candidates() {
        let board = VariantPosition::default();
        assert!(pocket_items(&board, Color::White).is_empty());
        assert!(pocket_items(&board, Color::Black).is_empty());
        assert!(drop_squares(&board, Role::Knight).is_empty());

        let mut app =
            app_from_fen("rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1");
        app.sync_pockets();
        assert!(app.ui.pockets(Color::White).is_empty());
        assert!(app.ui.pockets(Color::Black).is_empty());
        // Board clicks keep working exactly as before.
        let (x, y) = square_center(&app, Square::E2);
        app.handle_board_click(x, y);
        assert_eq!(app.ui.selected_square(), Some(Square::E2));
        assert_eq!(app.ui.selected_pocket_piece(), None);
    }

    #[test]
    fn drops_are_identified_by_role_and_destination() {
        let board = position_from_fen("crazyhouse", "4k3/8/8/8/8/8/8/4K3[Q] w - - 0 1")
            .expect("crazyhouse fen");
        let targets = drop_squares(&board, Role::Queen);
        assert!(targets.contains(&Square::D4));
        // Only empty squares: the kings' squares are never drop targets.
        assert!(!targets.contains(&Square::E1));
        assert!(!targets.contains(&Square::E8));

        let mv = find_drop(&board, Role::Queen, Square::D4).expect("legal drop");
        assert!(matches!(
            mv,
            Move::Put { role: Role::Queen, to: Square::D4 }
        ));
        assert_eq!(mv.from(), None, "drops have no origin square");

        let mut after = board.clone();
        after.play_unchecked(mv);
        assert_eq!(
            after.board().piece_at(Square::D4).map(|p| (p.role, p.color)),
            Some((Role::Queen, Color::White))
        );
        let pockets = after.pockets().expect("crazyhouse pockets");
        assert_eq!(*pockets.get(Color::White).get(Role::Queen), 0);
    }

    #[test]
    fn pocket_click_then_target_click_drops_the_piece() {
        let mut app = app_from_crazyhouse_fen("4k3/8/8/8/8/8/8/4K3[Q] w - - 0 1");
        app.sync_pockets();
        let icon = *app.ui.pockets(Color::White).first().expect("pocket queen");
        assert_eq!(icon.role, Role::Queen);

        // Clicking the pocket icon clears any board selection and lights up
        // the legal landing squares.
        app.handle_board_click(icon.rect.x + icon.rect.w * 0.5, icon.rect.y + icon.rect.h * 0.5);
        assert_eq!(app.ui.selected_square(), None);
        assert_eq!(app.ui.selected_pocket_piece(), Some(Role::Queen));
        assert!(app.ui.is_move_target(Square::D4));

        // Clicking a highlighted square plays the drop through apply_move.
        let (x, y) = square_center(&app, Square::D4);
        app.handle_board_click(x, y);
        assert_eq!(
            app.board.board().piece_at(Square::D4).map(|p| (p.role, p.color)),
            Some((Role::Queen, Color::White))
        );
        let pockets = app.board.pockets().expect("crazyhouse pockets");
        assert_eq!(*pockets.get(Color::White).get(Role::Queen), 0);
        assert_eq!(app.ui.selected_pocket_piece(), None);
        assert!(!app.ui.is_move_target(Square::D4));
        assert_eq!(app.history.last().map(String::as_str), Some("Q@d4"));
        assert_eq!(app.board.turn(), Color::Black);
    }

    #[test]
    fn dragging_from_the_pocket_drops_on_a_target() {
        let mut app = app_from_crazyhouse_fen("r3k3/8/8/8/8/8/8/4K3[N] w - - 0 1");
        app.sync_pockets();
        let icon = *app.ui.pockets(Color::White).first().expect("pocket knight");
        let (px, py) = (icon.rect.x + icon.rect.w * 0.5, icon.rect.y + icon.rect.h * 0.5);

        app.handle_mouse_press(px, py);
        assert_eq!(app.ui.selected_pocket_piece(), Some(Role::Knight));
        assert_eq!(app.ui.dragging_pocket_piece(), Some(Role::Knight));
        assert_eq!(app.ui.dragging_square(), None);

        let (x, y) = square_center(&app, Square::F6);
        assert!(app.handle_cursor_moved(x, y));
        assert!(app.handle_mouse_release(x, y));

        assert_eq!(
            app.board.board().piece_at(Square::F6).map(|p| (p.role, p.color)),
            Some((Role::Knight, Color::White))
        );
        let pockets = app.board.pockets().expect("crazyhouse pockets");
        assert_eq!(*pockets.get(Color::White).get(Role::Knight), 0);
        assert!(!app.ui.is_dragging());
        assert_eq!(app.ui.selected_pocket_piece(), None);
        assert_eq!(app.board.turn(), Color::Black);
    }

    #[test]
    fn pocket_drag_released_elsewhere_cancels_without_moving() {
        let mut app = app_from_crazyhouse_fen("r3k3/8/8/8/8/8/8/4K3[N] w - - 0 1");
        app.sync_pockets();
        let icon = *app.ui.pockets(Color::White).first().expect("pocket knight");
        let (px, py) = (icon.rect.x + icon.rect.w * 0.5, icon.rect.y + icon.rect.h * 0.5);

        // Released outside the board: nothing is dropped and the pocket
        // selection clears.
        app.handle_mouse_press(px, py);
        app.handle_cursor_moved(2.0, 2.0);
        app.handle_mouse_release(2.0, 2.0);
        assert!(!app.ui.is_dragging());
        assert_eq!(app.ui.selected_pocket_piece(), None);
        assert_eq!(app.board.turn(), Color::White);
        let pockets = app.board.pockets().expect("crazyhouse pockets");
        assert_eq!(*pockets.get(Color::White).get(Role::Knight), 1);

        // Released on an occupied, non-target square: still no drop.
        app.handle_mouse_press(px, py);
        let (x, y) = square_center(&app, Square::E1);
        app.handle_cursor_moved(x, y);
        app.handle_mouse_release(x, y);
        assert!(!app.ui.is_dragging());
        assert_eq!(app.ui.selected_pocket_piece(), None);
        assert_eq!(
            app.board.board().piece_at(Square::E1).map(|p| p.role),
            Some(Role::King)
        );
        let pockets = app.board.pockets().expect("crazyhouse pockets");
        assert_eq!(*pockets.get(Color::White).get(Role::Knight), 1);
    }

    #[test]
    fn only_the_side_to_move_has_a_usable_local_pocket() {
        let mut app = app_from_crazyhouse_fen("4k3/8/8/8/8/8/8/4K3[Nn] b - - 0 1");
        app.sync_pockets();
        assert!(app.ui.pockets(Color::Black).iter().all(|p| p.interactive));
        assert!(app.ui.pockets(Color::White).iter().all(|p| !p.interactive));
        // The dimmed pocket does not even hit-test.
        let icon = *app.ui.pockets(Color::White).first().expect("pocket knight");
        app.handle_board_click(
            icon.rect.x + icon.rect.w * 0.5,
            icon.rect.y + icon.rect.h * 0.5,
        );
        assert_eq!(app.ui.selected_pocket_piece(), None);
    }

    #[test]
    fn live_crazyhouse_pockets_are_display_only() {
        let mut app = app_from_crazyhouse_fen("4k3/8/8/8/8/8/8/4K3[Q] w - - 0 1");
        let live = app.board.clone();
        app.mode = Mode::Live;
        app.live_board = Some(live);
        app.sync_pockets();

        assert_eq!(app.ui.pockets(Color::White).len(), 1);
        assert!(app.ui.pockets(Color::White).iter().all(|p| !p.interactive));
        let icon = *app.ui.pockets(Color::White).first().expect("pocket queen");
        app.handle_board_click(icon.rect.x + icon.rect.w * 0.5, icon.rect.y + icon.rect.h * 0.5);
        assert_eq!(app.ui.selected_pocket_piece(), None);
        assert!(!app.ui.is_dragging());
    }

    #[test]
    fn atomic_explosion_radius_is_the_moore_neighbourhood() {
        // An interior capture touches nine squares.
        let e4 = explosion_squares(Square::E4);
        assert_eq!(e4.len(), 9);
        for sq in [
            Square::E4,
            Square::D3,
            Square::D4,
            Square::D5,
            Square::E3,
            Square::E5,
            Square::F3,
            Square::F4,
            Square::F5,
        ] {
            assert!(e4.contains(&sq), "missing {sq:?}");
        }

        // Edge and corner blasts clip to the board instead of wrapping.
        assert_eq!(explosion_squares(Square::A1).len(), 4);
        assert_eq!(explosion_squares(Square::H4).len(), 6);
        assert!(!explosion_squares(Square::A1).contains(&Square::H1));
    }

    #[test]
    fn atomic_capture_reports_the_blast_and_removed_pieces() {
        // exd5 with a black knight on c6 (explodes) and a white pawn on e6
        // (pawns are immune to Atomic explosions and must stay solid).
        let prev = position_from_fen(
            "atomic",
            "4k3/8/2n1P3/3p4/4P3/8/8/4K3 w - - 0 1",
        )
        .expect("atomic fen");
        let mv = find_move(&prev, Square::E4, Square::D5);
        let mut next = prev.clone();
        next.play_unchecked(mv);

        let explosion = explosion_for(&prev, &next, mv).expect("atomic capture explodes");
        assert_eq!(explosion.squares.len(), 9);
        for sq in [Square::D5, Square::C6, Square::E6, Square::C4, Square::E4] {
            assert!(explosion.squares.contains(&sq), "missing {sq:?}");
        }

        let captured = explosion
            .ghosts
            .iter()
            .find(|ghost| ghost.square == Square::D5)
            .expect("captured pawn ghost");
        assert!(captured.captured, "the piece on the destination fades faster");
        assert_eq!((captured.role, captured.color), (Role::Pawn, Color::Black));

        let knight = explosion
            .ghosts
            .iter()
            .find(|ghost| ghost.square == Square::C6)
            .expect("adjacent knight ghost");
        assert!(!knight.captured);
        assert_eq!(knight.role, Role::Knight);

        // Neither the mover's origin nor the immune pawn becomes a ghost;
        // both survive shakmaty's blast.
        assert!(!explosion.ghosts.iter().any(|ghost| ghost.square == Square::E4));
        assert!(!explosion.ghosts.iter().any(|ghost| ghost.square == Square::E6));
        assert_eq!(
            next.board().piece_at(Square::E6).map(|p| (p.role, p.color)),
            Some((Role::Pawn, Color::White))
        );
    }

    #[test]
    fn only_atomic_captures_explode() {
        let quiet_prev =
            position_from_fen("atomic", "4k3/8/2n5/3p4/4P3/8/8/4K3 w - - 0 1")
                .expect("atomic fen");
        let quiet = find_move(&quiet_prev, Square::E4, Square::E5);
        let mut quiet_next = quiet_prev.clone();
        quiet_next.play_unchecked(quiet);
        assert!(explosion_for(&quiet_prev, &quiet_next, quiet).is_none());

        let standard = position_from_fen(
            "standard",
            "r3k3/8/8/3p4/4P3/8/8/4K3 w - - 0 1",
        )
        .expect("standard fen");
        let capture = find_move(&standard, Square::E4, Square::D5);
        let mut next = standard.clone();
        next.play_unchecked(capture);
        assert!(
            explosion_for(&standard, &next, capture).is_none(),
            "captures outside Atomic stay quiet"
        );
    }

    #[test]
    fn explosion_alpha_fades_over_its_window() {
        assert!((explosion_flash(0.0) - 0.8).abs() < 1e-6);
        assert!((explosion_flash(250.0) - 0.6).abs() < 1e-6);
        assert!((explosion_flash(500.0) - 0.0).abs() < 1e-6);
        assert_eq!(explosion_flash(600.0), 0.0);
        assert!(explosion_flash(100.0) < explosion_flash(50.0));
        assert!(explosion_flash(300.0) < explosion_flash(100.0));

        assert!((explosion_piece_alpha(0.0, false) - 1.0).abs() < 1e-6);
        assert!((explosion_piece_alpha(250.0, false) - 0.5).abs() < 1e-6);
        assert_eq!(explosion_piece_alpha(500.0, false), 0.0);
        assert!(explosion_piece_alpha(600.0, false) == 0.0);
        // The captured piece goes over 200 ms, so it is half gone at 100 ms.
        assert!((explosion_piece_alpha(100.0, true) - 0.5).abs() < 1e-6);
        assert_eq!(explosion_piece_alpha(200.0, true), 0.0);
    }

    #[test]
    fn local_atomic_capture_starts_a_blast_and_a_second_one_overwrites_it() {
        // White c4/e4 pawns, Black b5/d5 pawns. Atomic captures explode both
        // the capturing and the captured piece, so exd5 empties d5 while the
        // adjacent c4 pawn survives (pawns are immune to the blast).
        let mut app = app_from_atomic_fen("4k3/8/8/1p1p4/2P1P3/8/8/4K3 w - - 0 1");
        let first = find_move(&app.board, Square::E4, Square::D5);
        app.apply_move(first);

        let first_blast = app.explosion.as_ref().expect("first blast");
        let first_squares = first_blast.squares.clone();
        assert!(first_squares.contains(&Square::D5));
        assert_eq!(
            first_blast
                .ghosts
                .iter()
                .map(|ghost| ghost.square)
                .collect::<Vec<_>>(),
            vec![Square::D5],
            "the captured pawn is the only removed piece; c4 is immune"
        );
        assert!(app.board.board().piece_at(Square::D5).is_none());
        assert_eq!(
            app.board.board().piece_at(Square::C4).map(|p| (p.role, p.color)),
            Some((Role::Pawn, Color::White)),
            "shakmaty keeps pawns out of the blast"
        );

        // Black takes back on c4: a second blast at a different centre.
        let second = find_move(&app.board, Square::B5, Square::C4);
        app.apply_move(second);

        let second_blast = app.explosion.as_ref().expect("second blast");
        assert_ne!(second_blast.squares, first_squares, "the new blast wins");
        assert!(second_blast.squares.contains(&Square::C4));
        assert!(second_blast.squares.contains(&Square::B3));
        assert!(!second_blast.squares.contains(&Square::E5));
        assert_eq!(
            second_blast
                .ghosts
                .iter()
                .map(|ghost| ghost.square)
                .collect::<Vec<_>>(),
            vec![Square::C4]
        );
        // The board is the authoritative result: both pawns at the second
        // blast are gone and no stale first-blast ghost is left behind.
        assert!(app.board.board().piece_at(Square::C4).is_none());
        assert!(app.board.board().piece_at(Square::B5).is_none());
        assert!(app.board.board().piece_at(Square::D5).is_none());
    }

    #[test]
    fn an_explosion_expires_after_its_window() {
        let mut app = app_from_atomic_fen("4k3/8/8/3p4/4P3/8/8/4K3 w - - 0 1");
        let mv = find_move(&app.board, Square::E4, Square::D5);
        app.apply_move(mv);
        assert!(app.explosion.is_some());
        assert!(app.tick_explosion().is_some(), "a fresh blast is animating");

        // Pretend the effect started more than 500 ms ago.
        if let Some(explosion) = app.explosion.as_mut() {
            explosion.start = Instant::now() - Duration::from_millis(600);
        }
        assert_eq!(app.tick_explosion(), None);
        assert!(app.explosion.is_none(), "the expired blast is dropped");
    }

    #[test]
    fn live_atomic_capture_derives_the_blast() {
        let prev = position_from_fen("atomic", "4k3/8/8/3p4/4P3/8/8/4K3 w - - 0 1")
            .expect("atomic fen");
        let mv = find_move(&prev, Square::E4, Square::D5);
        let mut next = prev.clone();
        next.play_unchecked(mv);

        let mut app = app_from_atomic_fen("4k3/8/8/3p4/4P3/8/8/4K3 b - - 0 1");
        app.mode = Mode::Live;
        app.live_board = Some(prev);
        app.note_live_move(&next);

        let explosion = app.explosion.as_ref().expect("TV capture explodes");
        assert!(explosion.squares.contains(&Square::D5));
        assert!(explosion.ghosts.iter().any(|ghost| ghost.square == Square::D5));

        // A quiet TV move clears the blast rather than leaving it stale.
        let quiet_prev = next;
        let quiet = find_move(&quiet_prev, Square::E8, Square::E7);
        let mut quiet_next = quiet_prev.clone();
        quiet_next.play_unchecked(quiet);
        app.live_board = Some(quiet_prev);
        app.note_live_move(&quiet_next);
        assert!(app.explosion.is_none());
    }

    #[test]
    fn background_tv_captures_do_not_explode_local_games() {
        let prev = position_from_fen("atomic", "4k3/8/8/3p4/4P3/8/8/4K3 w - - 0 1")
            .expect("atomic fen");
        let mv = find_move(&prev, Square::E4, Square::D5);
        let mut next = prev.clone();
        next.play_unchecked(mv);

        let mut app = app_from_atomic_fen("4k3/8/8/8/8/8/8/4K3 w - - 0 1");
        app.live_board = Some(prev);
        app.note_live_move(&next);
        assert!(
            app.explosion.is_none(),
            "the Live background stream must not animate Local play"
        );
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
        let (light, dark) = ui::BoardTheme::Brown.colors();
        // a1 is a dark square, so its file letter uses the light tint.
        assert_eq!(text_named(&texts, "a").color, [light[0], light[1], light[2], 1.0]);
        // b1 is light, so its letter uses the dark tint.
        assert_eq!(text_named(&texts, "b").color, [dark[0], dark[1], dark[2], 1.0]);
    }

    #[test]
    fn switching_the_config_retints_the_board_and_coordinates() {
        let mut app = app_with_live_game();
        assert_eq!(app.config, config::Config::default());
        assert_eq!(app.ui.board_theme(), ui::BoardTheme::Brown);

        app.apply_config(config::Config {
            board: ui::BoardTheme::Wood,
            pieces: renderer::PieceSet::Leipzig,
        });

        assert_eq!(app.config.board, ui::BoardTheme::Wood);
        assert_eq!(app.ui.board_theme(), ui::BoardTheme::Wood);
        // No renderer in unit tests, so the piece swap is only recorded.
        assert_eq!(app.config.pieces, renderer::PieceSet::Leipzig);

        let texts = app.build_texts();
        let (light, dark) = ui::BoardTheme::Wood.colors();
        assert_eq!(text_named(&texts, "a").color, [light[0], light[1], light[2], 1.0]);
        assert_eq!(text_named(&texts, "b").color, [dark[0], dark[1], dark[2], 1.0]);
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
        app.online = OnlineState::Active(Box::new(OnlineGame {
            game_id: "test".to_string(),
            initial_fen: "rnb1kbn1/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQ - 0 1".to_string(),
            board: position_from_fen(
                "standard",
                "rnb1kbn1/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQ - 0 1",
            )
            .expect("capture fen"),
            server_board: position_from_fen(
                "standard",
                "rnb1kbn1/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQ - 0 1",
            )
            .expect("capture fen"),
            server_moves: Vec::new(),
            pending: None,
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

    /// A `gameFull` frame with White = "me" and no moves yet.
    fn online_full() -> lichess::GameFull {
        lichess::GameFull {
            id: "abcd1234".to_string(),
            initial_fen: Some("startpos".to_string()),
            white: lichess::BoardPlayer {
                id: "me".to_string(),
                name: "me".to_string(),
                title: None,
                rating: Some(1500),
            },
            black: lichess::BoardPlayer {
                id: "them".to_string(),
                name: "them".to_string(),
                title: None,
                rating: Some(1600),
            },
            clock: Some(lichess::BoardClock {
                initial: 300_000,
                increment: 2_000,
            }),
            state: lichess::GameState {
                moves: String::new(),
                wtime: 300_000,
                btime: 300_000,
                status: "started".to_string(),
                winner: None,
            },
            rated: false,
        }
    }

    fn online_state(moves: &str, status: &str, winner: Option<&str>) -> lichess::GameState {
        lichess::GameState {
            moves: moves.to_string(),
            wtime: 299_000,
            btime: 299_000,
            status: status.to_string(),
            winner: winner.map(str::to_string),
        }
    }

    #[test]
    fn online_challenge_moves_from_challenging_to_waiting() {
        let mut app = app_from_fen("rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1");
        app.online = OnlineState::Challenging;
        app.on_online_challenge_created("abcd1234".to_string());
        assert!(matches!(
            app.online,
            OnlineState::WaitingAccept { ref challenge_id } if challenge_id == "abcd1234"
        ));
    }

    #[test]
    fn a_late_challenge_created_event_does_not_interrupt_an_active_game() {
        let mut app = app_from_fen("rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1");
        app.mode = Mode::Play;
        app.my_username = Some("me".to_string());
        app.on_online_full(online_full());
        app.on_online_challenge_created("abcd1234".to_string());
        assert!(matches!(app.online, OnlineState::Active(_)));
    }

    #[test]
    fn game_full_activates_the_game_with_my_colour() {
        let mut app = app_from_fen("rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1");
        app.mode = Mode::Play;
        app.my_username = Some("me".to_string());
        app.on_online_full(online_full());

        let OnlineState::Active(game) = &app.online else {
            panic!("expected Active, got a different state");
        };
        assert_eq!(game.my_color, Color::White);
        assert_eq!(game.white_rating, 1500);
        assert_eq!(game.black_name, "them");
        assert_eq!(game.white_clock, 300.0);
        assert!(!game.game_over);
        assert_eq!(app.online_game().map(|g| g.game_id.as_str()), Some("abcd1234"));
        assert!(app.can_select_piece(Color::White));
        assert!(!app.can_select_piece(Color::Black));
    }

    #[test]
    fn a_terminal_stream_state_finishes_the_game_and_keeps_the_board() {
        let mut app = app_from_fen("rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1");
        app.mode = Mode::Play;
        app.my_username = Some("me".to_string());
        app.on_online_full(online_full());
        app.on_online_state(online_state("e2e4 e7e5", "resign", Some("black")));

        let OnlineState::Finished(game) = &app.online else {
            panic!("expected Finished, got a different state");
        };
        assert!(game.game_over);
        assert_eq!(game.winner, Some(Color::Black));
        // The final position is still on screen and no longer playable.
        assert_eq!(app.online_game().map(|g| g.game_id.as_str()), Some("abcd1234"));
        assert!(!app.can_select_piece(Color::White));
        assert!(app.active_board().is_none());
    }

    #[test]
    fn failures_keep_the_last_board_but_stop_play() {
        let mut app = app_from_fen("rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1");
        app.mode = Mode::Play;
        app.my_username = Some("me".to_string());
        app.on_online_full(online_full());
        app.on_online_failed("Game stream failed".to_string());

        match &app.online {
            OnlineState::Failed { message, game } => {
                assert_eq!(message, "Game stream failed");
                assert!(game.is_some());
            }
            _ => panic!("expected Failed"),
        }
        assert!(app.online_game().is_some());
        assert!(!app.can_select_piece(Color::White));
    }

    #[test]
    fn the_challenge_panel_opens_in_play_and_keeps_the_typed_opponent() {
        let mut app = app_from_fen("rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1");
        app.challenge_opponent = "bob".to_string();
        app.open_challenge_panel();
        assert_eq!(app.mode, Mode::Play);
        assert!(app.ui.challenge_open());
        assert_eq!(app.challenge_opponent, "bob");
        // Pressing C during a live game must not replace its board with the
        // challenge panel.
        let mut active = app_from_fen("rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1");
        active.mode = Mode::Play;
        active.my_username = Some("me".to_string());
        active.on_online_full(online_full());
        active.open_challenge_panel();
        assert!(!active.ui.challenge_open());
        assert!(matches!(active.online, OnlineState::Active(_)));
    }

    #[test]
    fn challenge_panel_validation_and_selections_reach_the_request() {
        let mut app = app_from_fen("rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1");
        app.challenge_opponent.clear();
        assert!(app.panel_challenge_request().is_err());

        app.challenge_opponent = "bad name".to_string();
        assert!(app.panel_challenge_request().is_err());

        app.challenge_opponent = "  bob  ".to_string();
        app.ui.select_challenge_clock(2);
        app.ui.select_challenge_color(1);
        let request = app.panel_challenge_request().expect("valid request");
        assert_eq!(request.username(), "bob");
        assert_eq!(request.clock_limit(), 180);
        assert_eq!(request.clock_increment(), 2);
        assert_eq!(request.color(), lichess::ChallengeColor::Black);
        assert!(!request.rated());
    }

    #[test]
    fn challenge_keys_edit_the_opponent_field_and_cancel() {
        let mut app = app_from_fen("rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1");
        app.open_challenge_panel();
        app.challenge_opponent.clear();

        assert!(app.handle_challenge_key(KeyCode::KeyB, Some("b")));
        assert!(app.handle_challenge_key(KeyCode::KeyO, Some("o")));
        assert_eq!(app.challenge_opponent, "bo");

        // Spaces, punctuation and non-ASCII are refused; `_` and `-` are not.
        assert!(app.handle_challenge_key(KeyCode::Space, Some(" !ü-")));
        assert_eq!(app.challenge_opponent, "bo-");

        assert!(app.handle_challenge_key(KeyCode::Backspace, None));
        assert_eq!(app.challenge_opponent, "bo");

        app.challenge_opponent = "a".repeat(30);
        assert!(!app.handle_challenge_key(KeyCode::KeyB, Some("b")));
        assert_eq!(app.challenge_opponent.len(), 30);

        assert!(app.handle_challenge_key(KeyCode::Escape, None));
        assert!(!app.ui.challenge_open());
        // The text survives closing and reopening.
        app.open_challenge_panel();
        assert_eq!(app.challenge_opponent.len(), 30);
    }

    #[test]
    fn submitting_a_challenge_validates_before_changing_state() {
        let mut app = app_from_fen("rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1");
        app.open_challenge_panel();
        app.challenge_opponent = "x".to_string();
        app.handle_challenge_key(KeyCode::Enter, None);
        assert!(app.ui.challenge_open(), "invalid input keeps the panel");
        assert!(matches!(app.online, OnlineState::Idle));
        assert!(app.challenge_error.is_some());

        app.challenge_opponent = "bob".to_string();
        app.token = Some(lichess::Token::new("s3cret"));
        app.handle_challenge_key(KeyCode::Enter, None);
        assert!(!app.ui.challenge_open());
        assert!(matches!(app.online, OnlineState::Challenging));
        assert!(app.challenge_error.is_none());
        assert_eq!(
            app.token.as_ref().map(lichess::Token::as_str),
            Some("s3cret")
        );
    }

    #[test]
    fn challenge_clicks_pick_controls_and_dismiss_outside() {
        let mut app = app_from_fen("rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1");
        app.open_challenge_panel();
        app.challenge_opponent = "bob".to_string();

        let panel = app.ui.challenge_panel().expect("panel").clone();
        let center = |rect: ui::Rect| (rect.x + rect.w * 0.5, rect.y + rect.h * 0.5);
        let (x, y) = center(panel.clocks[4]);
        app.handle_challenge_click(x, y);
        assert_eq!(app.ui.challenge_clock_index(), 4);
        let (x, y) = center(panel.colors[1]);
        app.handle_challenge_click(x, y);
        assert_eq!(app.ui.challenge_color_index(), 1);

        // Outside the panel dismisses it without starting anything.
        app.handle_challenge_click(0.0, 0.0);
        assert!(!app.ui.challenge_open());
        assert!(matches!(app.online, OnlineState::Idle));

        // Create submits, so the panel closes and the state advances.
        app.open_challenge_panel();
        app.token = Some(lichess::Token::new("s3cret"));
        let (x, y) = center(panel.create);
        app.handle_challenge_click(x, y);
        assert!(!app.ui.challenge_open());
        assert!(matches!(app.online, OnlineState::Challenging));
    }

    #[test]
    fn action_buttons_exist_only_while_the_game_is_running() {
        let mut app = app_from_fen("rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1");
        app.mode = Mode::Play;
        app.my_username = Some("me".to_string());
        app.on_online_full(online_full());
        app.sync_controls();
        assert_eq!(app.ui.controls().len(), 3);

        app.on_online_state(online_state("", "resign", Some("black")));
        app.sync_controls();
        assert!(app.ui.controls().is_empty());
    }

    #[test]
    fn controls_report_an_optimistic_notice_and_ignore_finished_games() {
        let mut app = app_from_fen("rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1");
        app.mode = Mode::Play;
        app.my_username = Some("me".to_string());
        app.on_online_full(online_full());
        app.token = Some(lichess::Token::new("s3cret"));

        app.send_control(ui::ControlKind::Takeback);
        assert_eq!(app.online_notice.as_deref(), Some("Takeback offer sent"));
        app.send_control(ui::ControlKind::Draw);
        assert_eq!(app.online_notice.as_deref(), Some("Draw offer sent"));

        // A finished game keeps its board but cannot send anything.
        app.on_online_state(online_state("", "resign", Some("black")));
        app.online_notice = None;
        app.send_control(ui::ControlKind::Resign);
        assert!(app.online_notice.is_none());
    }

    fn app_in_online_game() -> App {
        let mut app = app_from_fen("rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1");
        app.mode = Mode::Play;
        app.my_username = Some("me".to_string());
        app.on_online_full(online_full());
        app.token = Some(lichess::Token::new("s3cret"));
        app
    }

    #[test]
    fn online_moves_apply_optimistically_and_confirm_without_double_applying() {
        let mut app = app_in_online_game();
        let mv = find_move(
            app.active_board_ref().expect("active board"),
            Square::E2,
            Square::E4,
        );
        app.apply_move(mv);

        // The board advances immediately; the server has not echoed it yet.
        {
            let game = app.online_game().expect("game");
            assert!(game.board.board().piece_at(Square::E4).is_some());
            assert!(game.board.board().piece_at(Square::E2).is_none());
            assert_eq!(game.board.turn(), Color::Black);
            assert!(game
                .pending
                .as_ref()
                .is_some_and(|pending| pending.uci == "e2e4" && pending.index == 0));
            assert!(game.server_moves.is_empty());
        }
        assert_eq!(app.history, vec!["e4".to_string()]);

        // A stale frame that does not yet contain the move keeps it on screen.
        app.on_online_state(online_state("", "started", None));
        {
            let game = app.online_game().expect("game");
            assert!(game.board.board().piece_at(Square::E4).is_some());
            assert!(game.pending.is_some());
        }
        assert_eq!(app.history, vec!["e4".to_string()]);

        // The echo confirms it; the board must not advance twice and the SAN
        // must not be appended twice.
        app.on_online_state(online_state("e2e4", "started", None));
        {
            let game = app.online_game().expect("game");
            assert!(game.pending.is_none());
            assert_eq!(game.server_moves, vec!["e2e4".to_string()]);
            assert_eq!(game.board.turn(), Color::Black);
            assert_eq!(game.board.board().piece_at(Square::E4).map(|p| p.role), Some(Role::Pawn));
        }
        assert_eq!(app.history, vec!["e4".to_string()]);

        // A duplicated frame changes nothing.
        app.on_online_state(online_state("e2e4", "started", None));
        assert_eq!(app.history, vec!["e4".to_string()]);
    }

    #[test]
    fn a_confirmed_move_with_a_reply_adds_each_san_once() {
        let mut app = app_in_online_game();
        let mv = find_move(
            app.active_board_ref().expect("active board"),
            Square::E2,
            Square::E4,
        );
        app.apply_move(mv);
        app.on_online_state(online_state("e2e4 e7e5", "started", None));

        let game = app.online_game().expect("game");
        assert!(game.pending.is_none());
        assert_eq!(game.board.turn(), Color::White);
        assert_eq!(app.history, vec!["e4".to_string(), "e5".to_string()]);
    }

    #[test]
    fn a_rejected_move_rolls_the_board_and_history_back() {
        let mut app = app_in_online_game();
        let mv = find_move(
            app.active_board_ref().expect("active board"),
            Square::E2,
            Square::E4,
        );
        app.apply_move(mv);

        // The server moved past our index with a different move: the
        // optimistic e4 never happened, so d4 is the real position.
        app.on_online_state(online_state("d2d4", "started", None));
        let game = app.online_game().expect("game");
        assert!(game.pending.is_none());
        assert!(game.board.board().piece_at(Square::E4).is_none());
        assert!(game.board.board().piece_at(Square::D4).is_some());
        assert_eq!(app.history, vec!["d4".to_string()]);
        assert_eq!(
            app.online_notice.as_deref(),
            Some("Move rejected — board restored")
        );
    }

    #[test]
    fn a_failed_move_post_rolls_back_without_waiting_for_the_stream() {
        let mut app = app_in_online_game();
        let mv = find_move(
            app.active_board_ref().expect("active board"),
            Square::E2,
            Square::E4,
        );
        app.apply_move(mv);
        app.on_online_move_rejected("Move failed: socket closed".to_string());

        let game = app.online_game().expect("game");
        assert!(game.pending.is_none());
        assert!(game.board.board().piece_at(Square::E2).is_some());
        assert!(game.board.board().piece_at(Square::E4).is_none());
        assert!(app.history.is_empty());
        assert_eq!(
            app.online_notice.as_deref(),
            Some("Move failed: socket closed")
        );
    }

    #[test]
    fn move_failure_notices_never_contain_the_token() {
        let mut app = app_in_online_game();
        let mv = find_move(
            app.active_board_ref().expect("active board"),
            Square::E2,
            Square::E4,
        );
        app.apply_move(mv);
        app.on_online_move_rejected("Move failed: Bearer s3cret rejected".to_string());
        let notice = app.online_notice.clone().expect("notice");
        assert!(!notice.contains("s3cret"), "{notice}");
        assert!(notice.contains("[redacted]"), "{notice}");
    }

    #[test]
    fn online_errors_become_short_user_facing_messages() {
        let token = lichess::Token::new("s3cret");

        let invalid = lichess::Error::InvalidInput {
            field: "username",
            message: "must be 2-30 characters".into(),
        };
        assert_eq!(
            online_error("Could not challenge bob", &invalid, &token),
            "Invalid username: must be 2-30 characters"
        );

        let limited = lichess::Error::RateLimited {
            retry_after: Some(Duration::from_secs(7)),
        };
        assert_eq!(
            online_error("Move failed", &limited, &token),
            "Lichess rate limit - retry in 7 s"
        );

        let protocol = lichess::Error::Protocol("unknown outcome".into());
        assert_eq!(
            online_error("Game stream failed", &protocol, &token),
            "Lichess sent an unexpected response"
        );
    }

    #[test]
    fn local_play_has_no_player_bars() {        let app = app_from_fen("rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1");
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
