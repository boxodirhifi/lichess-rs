//! Lichess HTTP clients.
//!
//! Two surfaces live here:
//!
//! * **Public TV** (`fetch_tv_channels`, `fetch_game_snapshot`, `stream_tv_feed`)
//!   — no auth, pinned to [`DEFAULT_BASE`].
//! * **Board API** ([`ApiClient`]) — token auth for account checks, challenges,
//!   the game stream, moves and (Stage 2) takeback/draw/resign.
//!
//! The Board API contract this crate implements:
//!
//! | Endpoint | Purpose |
//! |---|---|
//! | `GET /api/account` | validate a token, learn the account name |
//! | `POST /api/challenge/{username}` | create a challenge (form: `rated`, `clock.limit`, `clock.increment`, `color`, `keepAliveStream`); NDJSON: challenge object, then `{"done":…}` |
//! | `GET /api/challenge/{id}/show` | challenge status: `created`, `offline`, `canceled`, `declined`, `accepted` |
//! | `GET /api/board/game/{id}/stream` | NDJSON events: `gameFull`, `gameState`, `chatLine`, `opponentGone` |
//! | `POST /api/board/game/{id}/move/{uci}` | play a move |
//! | `POST /api/board/game/{id}/takeback/yes` | offer a takeback |
//! | `POST /api/board/game/{id}/draw/yes` | offer a draw |
//! | `POST /api/board/game/{id}/resign` | resign |
//!
//! Every authenticated endpoint expects `Authorization: Bearer <token>`.
//! [`ApiClient`] is pinned to `https://lichess.org` in production; the test
//! constructor accepts a local `http://127.0.0.1:…` base so request shapes and
//! retry behaviour can be tested without touching the network.

use crate::token::Token;
use crate::types::{
    Account, BoardStreamEvent, ChallengeCreated, ChallengeStatus, GameSnapshot, TvGame,
    TvStreamEvent,
};
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::time::Duration;

/// The one production API origin. TLS verification stays on (ureq default).
pub const DEFAULT_BASE: &str = "https://lichess.org";

/// How many times a rate-limited request is retried after the first attempt.
const MAX_RETRIES: usize = 3;
/// First backoff step; doubled per retry.
const BASE_BACKOFF: Duration = Duration::from_millis(250);
/// Upper bound for a single backoff sleep, even with a large `Retry-After`.
const MAX_BACKOFF: Duration = Duration::from_secs(5);

/// A typed error. The HTTP payload is boxed because `ureq::Error` is large and
/// would otherwise bloat every `Result` in the crate.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("lichess http error: {0}")]
    Http(Box<ureq::Error>),
    #[error("lichess io error: {0}")]
    Io(#[from] std::io::Error),
    /// The caller supplied something the Board API would reject. Caught before
    /// any request leaves the process.
    #[error("invalid {field}: {message}")]
    InvalidInput {
        field: &'static str,
        message: String,
    },
    /// HTTP 429. `retry_after` is the server's `Retry-After` hint, if any.
    #[error("lichess rate limit (retry after {retry_after:?})")]
    RateLimited { retry_after: Option<Duration> },
    /// A response body did not match the documented shape.
    #[error("malformed lichess response: {0}")]
    Parse(#[from] serde_json::Error),
    /// A response was well-formed JSON but not something this client
    /// understands (for example an unknown challenge outcome).
    #[error("unexpected lichess response: {0}")]
    Protocol(String),
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

    /// Whether this is a client-side validation failure (no request was sent).
    pub fn is_invalid_input(&self) -> bool {
        matches!(self, Error::InvalidInput { .. })
    }

    /// HTTP status for a response error, if the server answered. Transport
    /// failures (DNS, TLS, timeouts) and locally generated errors return
    /// `None`; rate limits are already summarized as [`Error::RateLimited`].
    pub fn http_status(&self) -> Option<u16> {
        match self {
            Error::Http(e) => match e.as_ref() {
                ureq::Error::Status(code, _) => Some(*code),
                ureq::Error::Transport(_) => None,
            },
            _ => None,
        }
    }
}

impl From<ureq::Error> for Error {
    fn from(e: ureq::Error) -> Self {
        map_ureq(e)
    }
}

fn map_ureq(e: ureq::Error) -> Error {
    match e {
        ureq::Error::Status(429, response) => Error::RateLimited {
            retry_after: retry_after(&response),
        },
        other => Error::Http(Box::new(other)),
    }
}

/// `Retry-After` is documented as seconds for Lichess; ignore malformed or
/// candidate values and fall back to the exponential schedule.
fn retry_after(response: &ureq::Response) -> Option<Duration> {
    response
        .header("Retry-After")
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
}

/// 250 ms, 500 ms, 1 s, 2 s … capped at [`MAX_BACKOFF`].
fn backoff(retry: usize) -> Duration {
    let millis = BASE_BACKOFF
        .as_millis()
        .saturating_mul(1u128 << retry.min(16));
    Duration::from_millis(millis.min(MAX_BACKOFF.as_millis()) as u64)
}

/// Run `attempt`, retrying only rate-limit errors. Split from the HTTP client
/// so the schedule can be unit-tested with a recording sleeper.
fn with_retries<T>(
    mut attempt: impl FnMut() -> Result<T, Error>,
    mut sleep: impl FnMut(Duration),
) -> Result<T, Error> {
    let mut retries = 0;
    loop {
        match attempt() {
            Ok(value) => return Ok(value),
            Err(Error::RateLimited { retry_after }) if retries < MAX_RETRIES => {
                let delay = retry_after
                    .unwrap_or_else(|| backoff(retries))
                    .min(MAX_BACKOFF);
                log::warn!("Lichess rate limit hit; retrying in {:?}", delay);
                sleep(delay);
                retries += 1;
            }
            Err(e) => return Err(e),
        }
    }
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

const USERNAME_MAX: usize = 30;
const CLOCK_LIMIT_MAX: u32 = 10_800;
const CLOCK_INCREMENT_MAX: u32 = 60;

fn invalid(field: &'static str, message: impl Into<String>) -> Error {
    Error::InvalidInput {
        field,
        message: message.into(),
    }
}

/// Lichess usernames: 2–30 ASCII letters, digits, `_` or `-`.
pub fn validate_username(username: &str) -> Result<(), Error> {
    if username.len() < 2 || username.len() > USERNAME_MAX {
        return Err(invalid(
            "username",
            format!("must be 2–{USERNAME_MAX} characters"),
        ));
    }
    if !username
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(invalid(
            "username",
            "only ASCII letters, digits, '_' and '-' are allowed",
        ));
    }
    Ok(())
}

/// Time control bounds accepted by the challenge endpoint. The limit is either
/// the documented short set (0, 15, 30, 45, 60, 90) or a multiple of 60 up to
/// three hours; the increment is 0–60.
pub fn validate_clock(limit_secs: u32, increment_secs: u32) -> Result<(), Error> {
    let limit_ok = limit_secs <= CLOCK_LIMIT_MAX
        && (limit_secs.is_multiple_of(60) || matches!(limit_secs, 15 | 30 | 45 | 90));
    if !limit_ok {
        return Err(invalid(
            "clock.limit",
            "must be 0, 15, 30, 45, 90 or a multiple of 60 up to 10800 seconds",
        ));
    }
    if increment_secs > CLOCK_INCREMENT_MAX {
        return Err(invalid(
            "clock.increment",
            format!("must be at most {CLOCK_INCREMENT_MAX} seconds"),
        ));
    }
    Ok(())
}

/// Lichess game ids are short base62 strings.
pub fn validate_game_id(id: &str) -> Result<(), Error> {
    if !(4..=16).contains(&id.len()) || !id.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(invalid(
            "game id",
            "must be 4–16 ASCII letters or digits",
        ));
    }
    Ok(())
}

/// Board API moves are UCI: `e2e4`, `e7e8q`, castling `e1g1`.
pub fn validate_uci(uci: &str) -> Result<(), Error> {
    let bytes = uci.as_bytes();
    let square = |file: u8, rank: u8| (b'a'..=b'h').contains(&file) && (b'1'..=b'8').contains(&rank);
    let ok = match bytes.len() {
        4 | 5 => {
            square(bytes[0], bytes[1])
                && square(bytes[2], bytes[3])
                && (bytes.len() == 4 || matches!(bytes[4], b'q' | b'r' | b'b' | b'n'))
        }
        _ => false,
    };
    if ok {
        Ok(())
    } else {
        Err(invalid("move", "expected UCI such as e2e4 or e7e8q"))
    }
}

/// Side the challenge creator wants. `Random` lets Lichess pick.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChallengeColor {
    White,
    Black,
    #[default]
    Random,
}

impl ChallengeColor {
    pub const ALL: [ChallengeColor; 3] = [
        ChallengeColor::White,
        ChallengeColor::Black,
        ChallengeColor::Random,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            ChallengeColor::White => "white",
            ChallengeColor::Black => "black",
            ChallengeColor::Random => "random",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "white" => Some(ChallengeColor::White),
            "black" => Some(ChallengeColor::Black),
            "random" => Some(ChallengeColor::Random),
            _ => None,
        }
    }
}

/// A validated challenge request. Construct with [`ChallengeRequest::new`]; the
/// fields are private so no unvalidated value can reach the HTTP layer.
#[derive(Debug, Clone)]
pub struct ChallengeRequest {
    username: String,
    rated: bool,
    clock_limit: u32,
    clock_increment: u32,
    color: ChallengeColor,
}

impl ChallengeRequest {
    pub fn new(
        username: impl Into<String>,
        rated: bool,
        clock_limit: u32,
        clock_increment: u32,
        color: ChallengeColor,
    ) -> Result<Self, Error> {
        let username = username.into();
        validate_username(&username)?;
        validate_clock(clock_limit, clock_increment)?;
        Ok(Self {
            username,
            rated,
            clock_limit,
            clock_increment,
            color,
        })
    }

    pub fn username(&self) -> &str {
        &self.username
    }

    pub fn rated(&self) -> bool {
        self.rated
    }

    pub fn clock_limit(&self) -> u32 {
        self.clock_limit
    }

    pub fn clock_increment(&self) -> u32 {
        self.clock_increment
    }

    pub fn color(&self) -> ChallengeColor {
        self.color
    }

    fn form(&self) -> Vec<(&'static str, String)> {
        vec![
            ("rated", self.rated.to_string()),
            ("clock.limit", self.clock_limit.to_string()),
            ("clock.increment", self.clock_increment.to_string()),
            ("color", self.color.as_str().to_string()),
            // Keep the realtime challenge open while we wait for an answer.
            ("keepAliveStream", "true".to_string()),
        ]
    }
}

/// How an outgoing challenge ended while its keep-alive stream was open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChallengeOutcome {
    Accepted,
    Declined,
    Canceled,
}

/// `{"done":"accepted"}` / `"declined"` / `"canceled"` challenge stream lines.
#[derive(serde::Deserialize)]
struct ChallengeDone {
    done: String,
}

// ---------------------------------------------------------------------------
// Authenticated client
// ---------------------------------------------------------------------------

/// Board API client. Production code goes through [`ApiClient::new`], which is
/// hard-wired to [`DEFAULT_BASE`]; the constructor is the only way to change
/// the origin and exists for unit tests.
#[derive(Clone)]
pub struct ApiClient {
    base: String,
    agent: ureq::Agent,
}

impl Default for ApiClient {
    fn default() -> Self {
        Self::new()
    }
}

impl ApiClient {
    /// Production client: `https://lichess.org` only, default TLS validation.
    pub fn new() -> Self {
        Self::with_base(DEFAULT_BASE)
    }

    fn with_base(base: impl Into<String>) -> Self {
        Self {
            base: base.into(),
            agent: ureq::AgentBuilder::new().build(),
        }
    }

    /// Test-only client pointed at a local `TcpListener`. Never compiled into
    /// the binary, so production cannot leave the pinned origin.
    #[cfg(test)]
    fn for_local_test(base: impl Into<String>) -> Self {
        Self::with_base(base)
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }

    fn get(&self, token: &Token, path: &str) -> ureq::Request {
        self.agent
            .get(&self.url(path))
            .set("Authorization", &format!("Bearer {}", token.as_str()))
            .set("Accept", "application/json")
    }

    fn post(&self, token: &Token, path: &str) -> ureq::Request {
        self.agent
            .post(&self.url(path))
            .set("Authorization", &format!("Bearer {}", token.as_str()))
            .set("Accept", "application/json")
    }

    /// Execute a request with the shared 429 retry schedule. TLS verification
    /// is ureq's default; nothing here can disable it.
    fn call(&self, request: ureq::Request) -> Result<ureq::Response, Error> {
        with_retries(|| request.clone().call().map_err(map_ureq), std::thread::sleep)
    }

    fn get_json<T: serde::de::DeserializeOwned>(
        &self,
        token: &Token,
        path: &str,
    ) -> Result<T, Error> {
        let body = self.call(self.get(token, path))?.into_string()?;
        Ok(serde_json::from_str(&body)?)
    }

    /// Public profile of the token's account; cheap way to validate a token.
    pub fn account_me(&self, token: &Token) -> Result<Account, Error> {
        self.get_json(token, "/api/account")
    }

    /// Create a real-time challenge and keep it alive until the opponent
    /// answers. `keepAliveStream=true` makes the endpoint NDJSON: the first
    /// line is the [`ChallengeCreated`] object, then the stream stays open
    /// (15 s server pings) until `{"done":"accepted"|"declined"|"canceled"}`.
    ///
    /// `on_created` fires as soon as the challenge exists, so the UI can show
    /// "waiting for acceptance" while this call blocks. The callback may run
    /// more than once only if a 429 forces a retry (nothing was created).
    pub fn create_challenge(
        &self,
        token: &Token,
        request: &ChallengeRequest,
        mut on_created: impl FnMut(&ChallengeCreated),
    ) -> Result<ChallengeOutcome, Error> {
        validate_username(request.username())?;
        validate_clock(request.clock_limit(), request.clock_increment())?;
        let form = request.form();
        let pairs: Vec<(&str, &str)> = form
            .iter()
            .map(|(key, value)| (*key, value.as_str()))
            .collect();
        let path = format!("/api/challenge/{}", request.username());
        // `send_form` performs the request, so rebuild it for every retry.
        with_retries(
            || {
                let response = self
                    .post(token, &path)
                    .set("Accept", "application/x-ndjson")
                    .send_form(&pairs)
                    .map_err(map_ureq)?;
                let mut lines = BufReader::new(response.into_reader()).lines();
                let first = lines.next().transpose()?.unwrap_or_default();
                let created: ChallengeCreated = serde_json::from_str(&first)?;
                on_created(&created);
                for line in lines {
                    let line = line?;
                    if line.trim().is_empty() {
                        continue;
                    }
                    let done: ChallengeDone = serde_json::from_str(&line)?;
                    return match done.done.as_str() {
                        "accepted" => Ok(ChallengeOutcome::Accepted),
                        "declined" => Ok(ChallengeOutcome::Declined),
                        "canceled" => Ok(ChallengeOutcome::Canceled),
                        other => Err(Error::Protocol(format!(
                            "unknown challenge outcome: {other}"
                        ))),
                    };
                }
                // The server closed without a verdict (a 2-minute timeout on
                // its side); treat it like a cancellation.
                Ok(ChallengeOutcome::Canceled)
            },
            std::thread::sleep,
        )
    }

    /// Current status of a challenge: `created`, `offline`, `canceled`,
    /// `declined` or `accepted`.
    pub fn fetch_challenge_status(
        &self,
        token: &Token,
        challenge_id: &str,
    ) -> Result<ChallengeStatus, Error> {
        validate_game_id(challenge_id)?;
        self.get_json(token, &format!("/api/challenge/{challenge_id}/show"))
    }

    /// Stream a Board API game. Calls `callback` for every event; the first is
    /// always the full `GameFull` frame. Returns when the game ends or the
    /// connection closes.
    pub fn stream_board_game(
        &self,
        token: &Token,
        game_id: &str,
        mut callback: impl FnMut(BoardStreamEvent),
    ) -> Result<(), Error> {
        validate_game_id(game_id)?;
        let response = self
            .agent
            .get(&self.url(&format!("/api/board/game/{game_id}/stream")))
            .set("Authorization", &format!("Bearer {}", token.as_str()))
            .set("Accept", "application/x-ndjson")
            .call()
            .map_err(map_ureq)?;
        for line in BufReader::new(response.into_reader()).lines() {
            let line = line?;
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<BoardStreamEvent>(&line) {
                Ok(event) => callback(event),
                // A single malformed frame must not kill a live game.
                Err(e) => log::warn!("Skipping malformed board stream event: {e}"),
            }
        }
        Ok(())
    }

    /// Play a validated UCI move. Requires the `board:play` scope.
    pub fn play_board_move(
        &self,
        token: &Token,
        game_id: &str,
        uci: &str,
    ) -> Result<(), Error> {
        validate_game_id(game_id)?;
        validate_uci(uci)?;
        self.call(self.post(token, &format!("/api/board/game/{game_id}/move/{uci}")))?;
        Ok(())
    }

    fn post_ok(&self, token: &Token, path: &str) -> Result<(), Error> {
        self.call(self.post(token, path))?;
        Ok(())
    }

    /// Offer a takeback (`takeback/yes`). The opponent still has to accept.
    pub fn takeback(&self, token: &Token, game_id: &str) -> Result<(), Error> {
        validate_game_id(game_id)?;
        self.post_ok(token, &format!("/api/board/game/{game_id}/takeback/yes"))
    }

    /// Offer a draw (`draw/yes`). The opponent still has to accept.
    pub fn offer_draw(&self, token: &Token, game_id: &str) -> Result<(), Error> {
        validate_game_id(game_id)?;
        self.post_ok(token, &format!("/api/board/game/{game_id}/draw/yes"))
    }

    /// Resign immediately.
    pub fn resign(&self, token: &Token, game_id: &str) -> Result<(), Error> {
        validate_game_id(game_id)?;
        self.post_ok(token, &format!("/api/board/game/{game_id}/resign"))
    }
}

// ---------------------------------------------------------------------------
// Public TV (no auth)
// ---------------------------------------------------------------------------

/// Fetch the currently featured TV games across channels.
pub fn fetch_tv_channels() -> Result<HashMap<String, TvGame>, Error> {
    let body = ureq::get(&format!("{DEFAULT_BASE}/api/tv/channels"))
        .set("Accept", "application/json")
        .call()?
        .into_string()?;
    Ok(serde_json::from_str(&body)?)
}

/// Export a single game's current state, including `lastFen`.
pub fn fetch_game_snapshot(game_id: &str) -> Result<GameSnapshot, Error> {
    validate_game_id(game_id)?;
    let body = ureq::get(&format!("{DEFAULT_BASE}/game/export/{game_id}"))
        .set("Accept", "application/json")
        .call()?
        .into_string()?;
    Ok(serde_json::from_str(&body)?)
}

/// Stream Lichess TV. A `channel` key (e.g. "atomic", "horde") selects the
/// per-channel feed `/api/tv/{channel}/feed`; `None` falls back to the
/// all-channels feed. `dwell` bounds how long the stream is held open: the
/// socket read errors out once the deadline passes and that is reported as
/// `Ok(())` so a single blocking thread can rotate channels without an async
/// runtime. Calls `callback` for each event.
pub fn stream_tv_feed(
    channel: Option<&str>,
    dwell: Duration,
    mut callback: impl FnMut(TvStreamEvent),
) -> Result<(), Error> {
    let url = match channel {
        Some(ch) => format!("{DEFAULT_BASE}/api/tv/{ch}/feed"),
        None => format!("{DEFAULT_BASE}/api/tv/feed"),
    };
    let agent = ureq::AgentBuilder::new().timeout(dwell).build();
    let response = agent
        .get(&url)
        .set("Accept", "application/x-ndjson")
        .call()?;

    for line in BufReader::new(response.into_reader()).lines() {
        let line = match line {
            Ok(line) => line,
            Err(e)
                if e.kind() == std::io::ErrorKind::TimedOut
                    || e.kind() == std::io::ErrorKind::WouldBlock =>
            {
                // The dwell deadline passed: normal rotation, not a failure.
                return Ok(());
            }
            Err(e) => return Err(e.into()),
        };
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::mpsc;

    // ---- validation -------------------------------------------------------

    #[test]
    fn usernames_are_bounded_and_restricted() {
        for ok in ["ab", "Dr_Nykterstein-1", &"a".repeat(30)] {
            assert!(validate_username(ok).is_ok(), "{ok:?}");
        }
        for bad in ["", "a", "bad name", "bad!", "ümlaut", &"a".repeat(31)] {
            assert!(validate_username(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn clock_bounds_are_enforced() {
        for ok in [(0, 0), (15, 0), (30, 1), (45, 2), (60, 3), (90, 0), (300, 60), (10_800, 0)] {
            assert!(validate_clock(ok.0, ok.1).is_ok(), "{ok:?}");
        }
        assert!(validate_clock(10_860, 0).is_err());
        assert!(validate_clock(7, 0).is_err());
        assert!(validate_clock(300, 61).is_err());
    }

    #[test]
    fn game_ids_are_short_alphanumeric() {
        assert!(validate_game_id("abcd1234").is_ok());
        assert!(validate_game_id("abc").is_err());
        assert!(validate_game_id("abcd-123").is_err());
        assert!(validate_game_id(&"a".repeat(17)).is_err());
    }

    #[test]
    fn uci_moves_are_validated_before_sending() {
        for ok in ["e2e4", "e7e8q", "e7e8n", "e1g1", "a1h8"] {
            assert!(validate_uci(ok).is_ok(), "{ok}");
        }
        for bad in ["", "e2e", "e2e44", "i2e4", "e2e9", "e7e8k", "e7e8Q", "N@e4"] {
            assert!(validate_uci(bad).is_err(), "{bad:?}");
        }
        assert!(validate_uci("N@e4").unwrap_err().is_invalid_input());
    }

    #[test]
    fn challenge_color_round_trips() {
        for color in ChallengeColor::ALL {
            assert_eq!(ChallengeColor::parse(color.as_str()), Some(color));
        }
        assert_eq!(ChallengeColor::parse("blue"), None);
    }

    #[test]
    fn challenge_requests_reject_bad_input() {
        assert!(ChallengeRequest::new("bob", false, 300, 2, ChallengeColor::Random).is_ok());
        assert!(ChallengeRequest::new("b", false, 300, 2, ChallengeColor::Random).is_err());
        assert!(ChallengeRequest::new("bob", false, 99_999, 2, ChallengeColor::Random).is_err());
        assert!(ChallengeRequest::new("bob", false, 301, 2, ChallengeColor::Random).is_err());
        assert!(ChallengeRequest::new("bob", false, 300, 999, ChallengeColor::Random).is_err());
    }

    // ---- backoff ----------------------------------------------------------

    #[test]
    fn rate_limits_retry_with_exponential_backoff() {
        let mut attempts = 0;
        let mut slept = Vec::new();
        let result = with_retries(
            || {
                attempts += 1;
                if attempts < 3 {
                    Err(Error::RateLimited { retry_after: None })
                } else {
                    Ok("done")
                }
            },
            |delay| slept.push(delay),
        );
        assert_eq!(result.unwrap(), "done");
        assert_eq!(attempts, 3);
        assert_eq!(
            slept,
            vec![Duration::from_millis(250), Duration::from_millis(500)]
        );
    }

    #[test]
    fn rate_limits_stop_after_the_retry_budget() {
        let mut attempts = 0;
        let mut slept = Vec::new();
        let result = with_retries::<()>(
            || {
                attempts += 1;
                Err(Error::RateLimited { retry_after: None })
            },
            |delay| slept.push(delay),
        );
        assert!(matches!(result, Err(Error::RateLimited { .. })));
        assert_eq!(attempts, MAX_RETRIES + 1);
        assert_eq!(slept.len(), MAX_RETRIES);
    }

    #[test]
    fn retry_after_overrides_the_backoff_and_is_capped() {
        let mut attempts = 0;
        let mut slept = Vec::new();
        let _ = with_retries::<()>(
            || {
                attempts += 1;
                Err(Error::RateLimited {
                    retry_after: Some(Duration::from_secs(60)),
                })
            },
            |delay| slept.push(delay),
        );
        assert_eq!(slept, vec![MAX_BACKOFF; MAX_RETRIES]);
    }

    #[test]
    fn non_rate_limit_errors_are_not_retried() {
        let mut attempts = 0;
        let result = with_retries::<()>(
            || {
                attempts += 1;
                Err(Error::InvalidInput {
                    field: "move",
                    message: "bad".into(),
                })
            },
            |_| panic!("must not sleep"),
        );
        assert!(result.is_err());
        assert_eq!(attempts, 1);
    }

    #[test]
    fn backoff_doubles_then_caps() {
        assert_eq!(backoff(0), Duration::from_millis(250));
        assert_eq!(backoff(1), Duration::from_millis(500));
        assert_eq!(backoff(2), Duration::from_millis(1000));
        assert_eq!(backoff(9), MAX_BACKOFF);
    }

    // ---- local HTTP test server ------------------------------------------

    struct Server {
        base: String,
        requests: mpsc::Receiver<String>,
        _thread: std::thread::JoinHandle<()>,
    }

    impl Server {
        /// Serve `responses` in order, one connection each. Every response is a
        /// complete HTTP/1.1 reply (status line + headers + body).
        fn start(responses: Vec<String>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
            let base = format!("http://{}", listener.local_addr().unwrap());
            let (tx, requests) = mpsc::channel();
            let thread = std::thread::spawn(move || {
                for response in responses {
                    let Ok((mut stream, _)) = listener.accept() else {
                        return;
                    };
                    let request = read_request(&mut stream);
                    let _ = tx.send(request);
                    let _ = stream.write_all(response.as_bytes());
                    let _ = stream.flush();
                }
            });
            Self {
                base,
                requests,
                _thread: thread,
            }
        }

        fn base(&self) -> &str {
            &self.base
        }

        fn next_request(&self) -> String {
            self.requests
                .recv_timeout(Duration::from_secs(5))
                .expect("expected a request")
        }
    }

    fn response(status: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn read_request(stream: &mut TcpStream) -> String {
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .ok();
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            match stream.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                Err(_) => break,
            }
            if let Some(head_end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&buf[..head_end]);
                let length = head
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        if name.eq_ignore_ascii_case("content-length") {
                            value.trim().parse::<usize>().ok()
                        } else {
                            None
                        }
                    })
                    .unwrap_or(0);
                if buf.len() >= head_end + 4 + length {
                    break;
                }
            }
        }
        String::from_utf8_lossy(&buf).to_string()
    }

    fn header<'a>(request: &'a str, name: &str) -> Option<&'a str> {
        request.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            if key.eq_ignore_ascii_case(name) {
                Some(value.trim())
            } else {
                None
            }
        })
    }

    // ---- client request/response shapes ----------------------------------

    #[test]
    fn account_check_sends_the_bearer_header_and_parses_json() {
        let server = Server::start(vec![response(
            "200 OK",
            r#"{"id":"me","username":"Me","title":null,"online":true}"#,
        )]);
        let client = ApiClient::for_local_test(server.base());
        let token = Token::new("s3cret");

        let account = client.account_me(&token).expect("account");
        assert_eq!(account.id, "me");
        assert_eq!(account.username, "Me");

        let request = server.next_request();
        let first = request.lines().next().unwrap();
        assert_eq!(first, "GET /api/account HTTP/1.1");
        assert_eq!(header(&request, "authorization"), Some("Bearer s3cret"));
    }

    #[test]
    fn challenge_creation_streams_the_challenge_and_keeps_it_alive() {
        let body = format!(
            "{}\n{}\n",
            r#"{"id":"abcd1234","url":"https://lichess.org/abcd1234","status":"created"}"#,
            r#"{"done":"accepted"}"#,
        );
        let server = Server::start(vec![response("200 OK", &body)]);
        let client = ApiClient::for_local_test(server.base());
        let token = Token::new("s3cret");
        let challenge =
            ChallengeRequest::new("bob", false, 300, 2, ChallengeColor::White).expect("valid");

        let mut created_id = None;
        let outcome = client
            .create_challenge(&token, &challenge, |created| {
                created_id = Some(created.id.clone())
            })
            .expect("challenge");
        assert_eq!(outcome, ChallengeOutcome::Accepted);
        assert_eq!(created_id.as_deref(), Some("abcd1234"));

        let request = server.next_request();
        assert_eq!(
            request.lines().next().unwrap(),
            "POST /api/challenge/bob HTTP/1.1"
        );
        assert_eq!(header(&request, "authorization"), Some("Bearer s3cret"));
        assert_eq!(header(&request, "accept"), Some("application/x-ndjson"));
        let body = request.split("\r\n\r\n").nth(1).unwrap();
        assert!(body.contains("rated=false"), "{body}");
        assert!(body.contains("clock.limit=300"), "{body}");
        assert!(body.contains("clock.increment=2"), "{body}");
        assert!(body.contains("color=white"), "{body}");
        assert!(body.contains("keepAliveStream=true"), "{body}");
    }

    #[test]
    fn declined_and_canceled_challenges_are_reported() {
        for (done, expected) in [
            ("declined", ChallengeOutcome::Declined),
            ("canceled", ChallengeOutcome::Canceled),
        ] {
            let body = format!(
                "{}\n{{\"done\":\"{done}\"}}\n",
                r#"{"id":"abcd1234","url":"https://lichess.org/abcd1234","status":"created"}"#,
            );
            let server = Server::start(vec![response("200 OK", &body)]);
            let client = ApiClient::for_local_test(server.base());
            let challenge =
                ChallengeRequest::new("bob", false, 300, 2, ChallengeColor::Random).expect("valid");
            let outcome = client
                .create_challenge(&Token::new("s3cret"), &challenge, |_| {})
                .expect("challenge stream");
            assert_eq!(outcome, expected);
        }
    }

    #[test]
    fn unknown_challenge_outcomes_are_protocol_errors() {
        let body = format!(
            "{}\n{}\n",
            r#"{"id":"abcd1234","url":null,"status":"created"}"#,
            r#"{"done":"exploded"}"#,
        );
        let server = Server::start(vec![response("200 OK", &body)]);
        let client = ApiClient::for_local_test(server.base());
        let challenge =
            ChallengeRequest::new("bob", false, 300, 2, ChallengeColor::Random).expect("valid");
        let error = client
            .create_challenge(&Token::new("s3cret"), &challenge, |_| {})
            .unwrap_err();
        assert!(matches!(error, Error::Protocol(_)), "{error:?}");
    }

    #[test]
    fn http_status_is_exposed_for_ui_messages() {
        let server = Server::start(vec![response("401 Unauthorized", "{}")]);
        let client = ApiClient::for_local_test(server.base());
        let error = client.account_me(&Token::new("s3cret")).unwrap_err();
        assert_eq!(error.http_status(), Some(401));
        assert!(!error.is_not_found());
        assert!(!error.is_invalid_input());
    }

    #[test]
    fn malformed_json_is_a_typed_parse_error() {
        let server = Server::start(vec![response("200 OK", "{not json")]);
        let client = ApiClient::for_local_test(server.base());
        let error = client.account_me(&Token::new("s3cret")).unwrap_err();
        assert!(matches!(error, Error::Parse(_)), "{error:?}");
        // The token never appears in the rendered error.
        assert!(!error.to_string().contains("s3cret"));
    }

    #[test]
    fn missing_required_fields_are_parse_errors() {
        let server = Server::start(vec![response("200 OK", r#"{"id":"me"}"#)]);
        let client = ApiClient::for_local_test(server.base());
        let error = client.account_me(&Token::new("s3cret")).unwrap_err();
        assert!(matches!(error, Error::Parse(_)), "{error:?}");
    }

    #[test]
    fn http_429_is_retried_and_honours_retry_after_zero() {
        let server = Server::start(vec![
            response("429 Too Many Requests", "{}").replace(
                "Content-Length: 2\r\n",
                "Content-Length: 2\r\nRetry-After: 0\r\n",
            ),
            response(
                "200 OK",
                r#"{"id":"me","username":"Me","title":null,"online":true}"#,
            ),
        ]);
        let client = ApiClient::for_local_test(server.base());
        let account = client.account_me(&Token::new("s3cret")).expect("retried");
        assert_eq!(account.username, "Me");
        // Two connections: the rate-limited request and the retry.
        let _first = server.next_request();
        let second = server.next_request();
        assert_eq!(
            second.lines().next().unwrap(),
            "GET /api/account HTTP/1.1"
        );
    }

    #[test]
    fn challenge_status_is_fetched_by_id() {
        let server = Server::start(vec![response(
            "200 OK",
            r#"{"id":"abcd1234","status":"declined"}"#,
        )]);
        let client = ApiClient::for_local_test(server.base());
        let status = client
            .fetch_challenge_status(&Token::new("s3cret"), "abcd1234")
            .expect("status");
        assert_eq!(status.status, "declined");
        assert_eq!(
            server.next_request().lines().next().unwrap(),
            "GET /api/challenge/abcd1234/show HTTP/1.1"
        );
    }

    #[test]
    fn illegal_moves_never_leave_the_process() {
        let server = Server::start(vec![]);
        let client = ApiClient::for_local_test(server.base());
        let error = client
            .play_board_move(&Token::new("s3cret"), "abcd1234", "e2e9")
            .unwrap_err();
        assert!(error.is_invalid_input());
        assert!(server.requests.try_recv().is_err(), "no request sent");
    }

    #[test]
    fn a_valid_move_posts_the_uci_path() {
        let server = Server::start(vec![response("200 OK", "{}")]);
        let client = ApiClient::for_local_test(server.base());
        client
            .play_board_move(&Token::new("s3cret"), "abcd1234", "e7e8q")
            .expect("move");
        let request = server.next_request();
        assert_eq!(
            request.lines().next().unwrap(),
            "POST /api/board/game/abcd1234/move/e7e8q HTTP/1.1"
        );
        assert_eq!(header(&request, "authorization"), Some("Bearer s3cret"));
    }

    #[test]
    fn the_game_stream_parses_ndjson_and_survives_bad_frames() {
        let body = format!(
            "{}\n{}\nnot-json\n{}\n",
            r#"{"type":"gameFull","id":"abcd1234","initialFen":"startpos","white":{"id":"w","name":"White","rating":1500},"black":{"id":"b","name":"Black","rating":1600},"clock":{"initial":300000,"increment":2000},"state":{"moves":"","wtime":300000,"btime":300000,"status":"started"},"rated":false}"#,
            r#"{"type":"gameState","moves":"e2e4","wtime":299000,"btime":300000,"status":"started"}"#,
            r#"{"type":"gameState","moves":"e2e4 e7e5","wtime":299000,"btime":299000,"status":"started"}"#,
        );
        let server = Server::start(vec![response("200 OK", &body)]);
        let client = ApiClient::for_local_test(server.base());

        let mut events = Vec::new();
        client
            .stream_board_game(&Token::new("s3cret"), "abcd1234", |event| {
                events.push(event)
            })
            .expect("stream");
        assert_eq!(events.len(), 3);
        assert!(matches!(events[0], BoardStreamEvent::Full(_)));
        assert!(matches!(events[1], BoardStreamEvent::State(_)));
        assert!(matches!(events[2], BoardStreamEvent::State(_)));

        let request = server.next_request();
        assert_eq!(
            request.lines().next().unwrap(),
            "GET /api/board/game/abcd1234/stream HTTP/1.1"
        );
        assert_eq!(
            header(&request, "accept"),
            Some("application/x-ndjson")
        );
    }

    #[test]
    fn control_endpoints_post_the_documented_paths() {
        let server = Server::start(vec![
            response("200 OK", r#"{"ok":true}"#),
            response("200 OK", r#"{"ok":true}"#),
            response("200 OK", r#"{"ok":true}"#),
        ]);
        let client = ApiClient::for_local_test(server.base());
        let token = Token::new("s3cret");

        client.takeback(&token, "abcd1234").expect("takeback");
        client.offer_draw(&token, "abcd1234").expect("draw");
        client.resign(&token, "abcd1234").expect("resign");

        let paths: Vec<String> = (0..3)
            .map(|_| {
                server
                    .next_request()
                    .lines()
                    .next()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(
            paths,
            vec![
                "POST /api/board/game/abcd1234/takeback/yes HTTP/1.1",
                "POST /api/board/game/abcd1234/draw/yes HTTP/1.1",
                "POST /api/board/game/abcd1234/resign HTTP/1.1",
            ]
        );
    }

    #[test]
    fn control_endpoints_validate_the_game_id_first() {
        let server = Server::start(vec![]);
        let client = ApiClient::for_local_test(server.base());
        let token = Token::new("s3cret");
        assert!(client.takeback(&token, "x").is_err());
        assert!(client.offer_draw(&token, "x").is_err());
        assert!(client.resign(&token, "x").is_err());
        assert!(server.requests.try_recv().is_err(), "no request sent");
    }

    #[test]
    fn invalid_game_ids_are_rejected_before_any_request() {
        let server = Server::start(vec![]);
        let client = ApiClient::for_local_test(server.base());
        let token = Token::new("s3cret");
        assert!(client.fetch_challenge_status(&token, "x").is_err());
        assert!(client.play_board_move(&token, "x", "e2e4").is_err());
        assert!(server.requests.try_recv().is_err(), "no request sent");
    }
}
