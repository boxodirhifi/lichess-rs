//! Board API tokens: owner-only storage, redacted formatting, and a helper for
//! masking secrets that may have slipped into an error string.
//!
//! Tokens are never logged, never included in config.toml, and never exposed
//! through `Debug`/`Display`. Read them with [`read_token`], which uses
//! `LICHESS_API_TOKEN` first and then the first line of
//! `~/.config/lichess-rs/token`.

use std::fmt;
use std::path::PathBuf;

/// An opaque Board API token. `Debug` and `Display` print `[redacted]`, so an
/// accidental `log::debug!("{token:?}")` cannot leak it.
#[derive(Clone)]
pub struct Token(String);

impl Token {
    /// Wrap a token string. The value is trimmed and must not be empty; callers
    /// that already validate emptiness can use this directly.
    pub fn new(token: impl Into<String>) -> Self {
        Self(token.into().trim().to_string())
    }

    /// The secret, for the `Authorization` header and redaction helpers. Do
    /// not log the returned value.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token([redacted])")
    }
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

/// Replace every occurrence of `secret` in `text` with `[redacted]`. Use this
/// on any error/debug string that might echo an upstream message before it is
/// logged or shown to the user.
pub fn redact(text: &str, secret: &str) -> String {
    if secret.is_empty() {
        return text.to_string();
    }
    text.replace(secret, "[redacted]")
}

/// `$XDG_CONFIG_HOME/lichess-rs/token`, falling back to
/// `~/.config/lichess-rs/token`.
fn token_path() -> Option<PathBuf> {
    let config_home = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|dir| !dir.is_empty())
        .or_else(|| std::env::var("HOME").ok().map(|home| format!("{home}/.config")))?;
    Some(PathBuf::from(config_home).join("lichess-rs").join("token"))
}

/// Read the Board API token: `LICHESS_API_TOKEN` first, then the first
/// non-empty line of the owner-only token file.
pub fn read_token() -> Option<Token> {
    if let Ok(token) = std::env::var("LICHESS_API_TOKEN") {
        let token = token.trim();
        if !token.is_empty() {
            return Some(Token::new(token));
        }
    }
    let file = token_path()?;
    std::fs::read_to_string(file)
        .ok()
        .and_then(|contents| {
            contents
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .map(Token::new)
        })
        .filter(|token| !token.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_and_display_never_reveal_the_secret() {
        let token = Token::new("lip_supersecret");
        assert!(!format!("{token:?}").contains("supersecret"));
        assert!(!format!("{token}").contains("supersecret"));
        assert_eq!(format!("{token:?}"), "Token([redacted])");
        assert_eq!(format!("{token}"), "[redacted]");
    }

    #[test]
    fn new_trims_whitespace() {
        let token = Token::new("  abc  ");
        assert_eq!(token.as_str(), "abc");
    }

    #[test]
    fn redact_masks_every_occurrence() {
        let text = "GET https://lichess.org?token=abc failed; header Bearer abc";
        let masked = redact(text, "abc");
        assert!(!masked.contains("abc"));
        assert_eq!(masked.matches("[redacted]").count(), 2);
    }

    #[test]
    fn redact_with_an_empty_secret_is_a_no_op() {
        assert_eq!(redact("nothing to hide", ""), "nothing to hide");
    }
}
