use crate::types::{GameSnapshot, TvGame};
use std::collections::HashMap;
use crate::types::TvStreamEvent;

const BASE: &str = "https://lichess.org";

#[derive(Debug)]
pub enum Error {
    Http(ureq::Error),
    Io(std::io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Http(e) => write!(f, "lichess http error: {}", e),
            Error::Io(e) => write!(f, "lichess io/parse error: {}", e),
        }
    }
}

impl std::error::Error for Error {}

impl From<ureq::Error> for Error {
    fn from(e: ureq::Error) -> Self {
        Error::Http(e)
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
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
}

use std::io::{BufRead, BufReader};

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
