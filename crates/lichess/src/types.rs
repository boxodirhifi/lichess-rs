use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct UserRef {
    pub id: Option<String>,
    pub name: Option<String>,
    pub title: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TvGame {
    #[serde(rename = "gameId")]
    pub game_id: Option<String>,
    pub user: Option<UserRef>,
    pub rating: Option<u32>,
    pub color: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PlayerInfo {
    pub user: Option<UserRef>,
    pub rating: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Players {
    pub white: Option<PlayerInfo>,
    pub black: Option<PlayerInfo>,
}

/// A lightweight snapshot of a game's current state.
#[derive(Debug, Clone, Deserialize)]
pub struct GameSnapshot {
    pub id: Option<String>,
    pub status: Option<String>,
    #[serde(rename = "lastFen")]
    pub last_fen: Option<String>,
    #[serde(rename = "lastMove")]
    pub last_move: Option<String>,
    pub moves: Option<String>,
    pub players: Option<Players>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StreamPlayer {
    pub color: String,
    pub user: StreamUser,
    pub rating: u32,
    pub seconds: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StreamUser {
    pub name: String,
    #[serde(default)]
    pub title: Option<String>,
    pub id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FeaturedData {
    pub id: String,
    pub orientation: String,
    pub players: Vec<StreamPlayer>,
    pub fen: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FenData {
    pub fen: String,
    pub lm: String,
    pub wc: u32,
    pub bc: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "t", content = "d")]
pub enum TvStreamEvent {
    #[serde(rename = "featured")]
    Featured(FeaturedData),
    #[serde(rename = "fen")]
    Fen(FenData),
}

/// The authenticated account, as returned by `GET /api/account/me`.
#[derive(Debug, Clone, Deserialize)]
pub struct Account {
    pub id: String,
    pub username: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub online: bool,
}

/// A player in a Board API game (`gameFull` stream line).
#[derive(Debug, Clone, Deserialize)]
pub struct BoardPlayer {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub rating: Option<u32>,
}

/// Initial clock settings of a Board API game, in milliseconds.
#[derive(Debug, Clone, Deserialize)]
pub struct BoardClock {
    pub initial: u64,
    pub increment: u64,
}

/// First event of `GET /api/board/game/{id}/stream`: the immutable frame,
/// plus the current `state`.
#[derive(Debug, Clone, Deserialize)]
pub struct GameFull {
    pub id: String,
    #[serde(rename = "initialFen", default)]
    pub initial_fen: Option<String>,
    pub white: BoardPlayer,
    pub black: BoardPlayer,
    #[serde(default)]
    pub clock: Option<BoardClock>,
    pub state: GameState,
    #[serde(default)]
    pub rated: bool,
}

/// Incremental game state: the full move list so far (UCI) and the clocks.
#[derive(Debug, Clone, Deserialize)]
pub struct GameState {
    /// All moves played so far, space separated, e.g. "e2e4 g8f6".
    pub moves: String,
    /// Milliseconds left on white's clock.
    pub wtime: u64,
    /// Milliseconds left on black's clock.
    pub btime: u64,
    /// Game status name, e.g. "started", "resign", "mate".
    pub status: String,
    #[serde(default)]
    pub winner: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChatLine {
    /// "player" or "spectator".
    #[serde(default)]
    pub room: Option<String>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OpponentGone {
    pub gone: bool,
}

/// One line of the Board API game stream.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum BoardStreamEvent {
    /// Boxed: the full frame is much larger than the other variants.
    #[serde(rename = "gameFull")]
    Full(Box<GameFull>),
    #[serde(rename = "gameState")]
    State(GameState),
    #[serde(rename = "chatLine")]
    Chat(ChatLine),
    #[serde(rename = "opponentGone")]
    OpponentGone(OpponentGone),
}

/// A created challenge, as returned by `POST /api/challenge/{username}`.
#[derive(Debug, Clone, Deserialize)]
pub struct ChallengeCreated {
    pub id: String,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChallengeAccepted {
    pub challenge: ChallengeCreated,
}
