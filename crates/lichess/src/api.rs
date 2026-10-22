use crate::types::{
    Account, BoardStreamEvent, ChallengeAccepted, ChallengeCreated, GameSnapshot, TvGame,
    TvStreamEvent,
};
use std::collections::HashMap;
use std::io::{BufRead, BufReader};

const BASE: &str = "https://lichess.org";

/// Errors are boxed: `ureq::Error` is very large (>270 bytes) and would
/// make every `Result` carry that payload around.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("lichess http error: {0}")]
    Http(Box<ureq::Error>),
    #[error("lichess io/parse error: {0}")]
    Io(#[from] std::io::Error),
}

impl Error {
    /// The Board API game stream 404s for challenges the opponent has not
    /// accepted yet; workers use this to distinguish "still waiting" from
    /// real failures.
    pub fn is_not_found(&self) -> bool {
        matches!(
            self,
            Error::Http(e) if matches!(e.as_ref(), ureq::Error::Status(404, _))
        )
    }
}

impl From<ureq::Error> for Error {
    fn from(e: ureq::Error) -> Self {
        Error::Http(Box::new(e))
    }
}

/// Fetch the currently featured TV games across channels (no auth required).
pub fn fetch_tv_channels() -> Result<HashMap<String, TvGame>, Error> {
    let resp = ureq::get(&format!("{}/api/tv/channels", BASE))
        .set("Accept", "application/json")
        .call()?;
    let map = resp.into_json::<HashMap<String, TvGame>>()?;
    Ok(map)
}

/// Export a single game's current state, including `lastFen`.
pub fn fetch_game_snapshot(game_id: &str) -> Result<GameSnapshot, Error> {
    let resp = ureq::get(&format!("{}/game/export/{}", BASE, game_id))
        .set("Accept", "application/json")
        .call()?;
    let snap = resp.into_json::<GameSnapshot>()?;
    Ok(snap)
}

/// Open a long-lived NDJSON stream to the Lichess TV feed.
/// Calls `callback` for each event. Returns `Err` when the stream ends.
pub fn stream_tv_feed(mut callback: impl FnMut(TvStreamEvent)) -> Result<(), Error> {
    let resp = ureq::get(&format!("{}/api/tv/feed", BASE))
        .set("Accept", "application/x-ndjson")
        .call()?;

    let reader = BufReader::new(resp.into_reader());
    for line in reader.lines() {
        let line = line?;
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<TvStreamEvent>(&line) {
            Ok(event) => callback(event),
            Err(e) => log::warn!("Failed to parse stream event: {}", e),
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// OAuth-protected endpoints (Board API)
// ---------------------------------------------------------------------------

/// Read an API token: the `LICHESS_API_TOKEN` environment variable first,
/// then the first line of `~/.config/lichess-rs/token` (XDG layout).
pub fn read_token() -> Option<String> {
    if let Ok(token) = std::env::var("LICHESS_API_TOKEN") {
        let token = token.trim().to_string();
        if !token.is_empty() {
            return Some(token);
        }
    }
    let config_home = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|dir| !dir.is_empty())
        .or_else(|| std::env::var("HOME").ok().map(|home| format!("{home}/.config")))?;
    let file = format!("{config_home}/lichess-rs/token");
    std::fs::read_to_string(file)
        .ok()
        .and_then(|contents| contents.lines().next().map(|line| line.trim().to_string()))
        .filter(|line| !line.is_empty())
}

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

/// Public profile of the token's account; cheap way to validate a token.
pub fn account_me(token: &str) -> Result<Account, Error> {
    let resp = ureq::get(&format!("{BASE}/api/account/me"))
        .set("Authorization", &bearer(token))
        .call()?;
    Ok(resp.into_json()?)
}

/// Create a real-time challenge against `username`. `clock_secs` and
/// `increment_secs` make up the time control (600/5 = "10+5"). The token
/// needs the `challenge:write` scope; the opponent still has to accept.
pub fn create_challenge(
    token: &str,
    username: &str,
    rated: bool,
    clock_secs: u32,
    increment_secs: u32,
    color: Option<&str>,
) -> Result<ChallengeCreated, Error> {
    let color = color.unwrap_or("");
    let pairs: Vec<(&str, String)> = vec![
        ("rated", rated.to_string()),
        ("clock.limit", clock_secs.to_string()),
        ("clock.increment", increment_secs.to_string()),
        ("color", color.to_string()),
    ];
    let form: Vec<(&str, &str)> = pairs
        .iter()
        .map(|(key, value)| (*key, value.as_str()))
        .filter(|(_, value)| !value.is_empty())
        .collect();
    let resp = ureq::post(&format!("{BASE}/api/challenge/{username}"))
        .set("Authorization", &bearer(token))
        .send_form(&form)?;
    let accepted: ChallengeAccepted = resp.into_json()?;
    Ok(accepted.challenge)
}

/// Stream the state of a Board API game. Calls `callback` for every event;
/// the first one is always the full `GameFull` frame. The stream ends when
/// the game ends.
pub fn stream_board_game(
    token: &str,
    game_id: &str,
    mut callback: impl FnMut(BoardStreamEvent),
) -> Result<(), Error> {
    let resp = ureq::get(&format!("{BASE}/api/board/game/{game_id}/stream"))
        .set("Authorization", &bearer(token))
        .set("Accept", "application/x-ndjson")
        .call()?;
    let reader = BufReader::new(resp.into_reader());
    for line in reader.lines() {
        let line = line?;
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<BoardStreamEvent>(&line) {
            Ok(event) => callback(event),
            Err(e) => log::warn!("Failed to parse board stream event: {}", e),
        }
    }
    Ok(())
}

/// Make a move in a Board API game, UCI notation ("e2e4" or "e7e8q").
/// Requires the `board:play` scope.
pub fn play_board_move(token: &str, game_id: &str, uci: &str) -> Result<(), Error> {
    let _ = ureq::post(&format!("{BASE}/api/board/game/{game_id}/move/{uci}"))
        .set("Authorization", &bearer(token))
        .call()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Requires network. Run with: cargo test -p lichess -- --ignored --nocapture
    #[test]
    #[ignore]
    fn live_lichess_connectivity() {
        let channels = fetch_tv_channels().expect("fetch tv channels");
        assert!(!channels.is_empty(), "expected at least one TV channel");

        for (name, game) in &channels {
            let player = game
                .user
                .as_ref()
                .and_then(|u| u.name.clone())
                .unwrap_or_else(|| "?".into());
            println!("[{}] {} (rating {:?}, game {:?})", name, player, game.rating, game.game_id);
        }

        // Prefer a channel that is almost certainly mid-game, not a fresh start.
        let game_id = channels
            .get("best")
            .or_else(|| channels.get("blitz"))
            .or_else(|| channels.get("rapid"))
            .and_then(|g| g.game_id.clone())
            .or_else(|| channels.values().find_map(|g| g.game_id.clone()))
            .expect("at least one game id");

        // Raw diagnostic: print the actual field names Lichess sends back.
        let raw = ureq::get(&format!("{}/game/export/{}", BASE, game_id))
            .set("Accept", "application/json")
            .call()
            .expect("raw fetch");
        let value: serde_json::Value = raw.into_json().expect("raw json");
        if let Some(obj) = value.as_object() {
            println!("---- raw fields for {} ----", game_id);
            for k in obj.keys() {
                println!("  {}", k);
            }
        }
        println!("raw lastFen:  {:?}", value.get("lastFen"));
        println!("raw lastMove: {:?}", value.get("lastMove"));
        println!("raw moves:    {:?}", value.get("moves"));

        // Typed snapshot.
        let snap = fetch_game_snapshot(&game_id).expect("typed snapshot");
        println!("---- typed snapshot ----");
        println!("status:   {:?}", snap.status);
        println!("lastMove: {:?}", snap.last_move);
        println!("lastFen:  {:?}", snap.last_fen);
        println!("moves:    {:?}", snap.moves.as_deref().map(|m| &m[..m.len().min(60)]));

        let has_moves = snap.moves.as_deref().map(|m| !m.is_empty()).unwrap_or(false);
        assert!(
            snap.last_fen.is_some() || has_moves,
            "expected a lastFen or a non-empty moves list"
        );
    }

    // Requires network + a valid token. Run with:
    //   LICHESS_API_TOKEN=... cargo test -p lichess -- --ignored --nocapture
    #[test]
    #[ignore]
    fn board_api_token_works() {
        let token = read_token().expect("LICHESS_API_TOKEN set or a token file");
        let me = account_me(&token).expect("account me with the token");
        assert!(!me.username.is_empty());
        println!("Token validates: account {} (id {})", me.username, me.id);
    }
}
