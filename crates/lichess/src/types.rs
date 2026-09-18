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
