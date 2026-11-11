//! User configuration: `~/.config/lichess-rs/config.toml` (or
//! `$XDG_CONFIG_HOME/lichess-rs/config.toml`).
//!
//! The file is optional. A missing file means defaults; a malformed one is
//! reported to the caller so startup can warn and fall back, and a hot-reload
//! can warn and keep the running config. The format is:
//!
//! ```toml
//! [theme]
//! board = "brown"      # light, dark, brown, wood
//! pieces = "cburnett"  # cburnett, merida, alpha, leipzig
//! ```

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;

use renderer::PieceSet;
use ui::BoardTheme;

/// The effective theme selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    pub board: BoardTheme,
    pub pieces: PieceSet,
}

impl Default for Config {
    /// Documented defaults: brown board, cburnett pieces.
    fn default() -> Self {
        Self {
            board: BoardTheme::Brown,
            pieces: PieceSet::Cburnett,
        }
    }
}

#[derive(Debug, Deserialize, Default)]
struct RawConfig {
    theme: Option<RawTheme>,
}

#[derive(Debug, Deserialize)]
struct RawTheme {
    board: Option<String>,
    pieces: Option<String>,
}

impl Config {
    /// Parse the TOML text. Unknown keys are ignored; unknown theme or piece
    /// names are errors (the caller decides whether to fall back or keep the
    /// running config).
    pub fn from_toml(text: &str) -> Result<Config> {
        let raw: RawConfig = toml::from_str(text).context("invalid config.toml")?;
        let defaults = Config::default();
        let Some(theme) = raw.theme else {
            return Ok(defaults);
        };

        let board = match theme.board.as_deref() {
            None => defaults.board,
            Some(name) => BoardTheme::from_name(name).ok_or_else(|| {
                anyhow!("unknown board theme {name:?} (expected light, dark, brown or wood)")
            })?,
        };
        let pieces = match theme.pieces.as_deref() {
            None => defaults.pieces,
            Some(name) => PieceSet::from_name(name).ok_or_else(|| {
                anyhow!("unknown piece set {name:?} (expected cburnett, merida, alpha or leipzig)")
            })?,
        };
        Ok(Config { board, pieces })
    }

    /// Read the file. `Ok(None)` means it does not exist (defaults apply);
    /// any other read or parse problem is an error.
    pub fn load(path: &Path) -> Result<Option<Config>> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        Config::from_toml(&text).map(Some)
    }
}

/// `$XDG_CONFIG_HOME/lichess-rs`, falling back to `~/.config/lichess-rs`.
pub fn config_dir() -> Option<PathBuf> {
    config_dir_from(
        std::env::var("XDG_CONFIG_HOME").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
    )
}

/// Pure half of [`config_dir`], so the XDG rules are testable without
/// touching the process environment.
pub fn config_dir_from(xdg_config_home: Option<&str>, home: Option<&str>) -> Option<PathBuf> {
    let base = match xdg_config_home {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => PathBuf::from(home?).join(".config"),
    };
    Some(base.join("lichess-rs"))
}

/// The watched config file path (`None` when neither XDG nor HOME is set).
pub fn config_path() -> Option<PathBuf> {
    config_dir().map(|dir| dir.join("config.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_brown_and_cburnett() {
        let config = Config::default();
        assert_eq!(config.board, BoardTheme::Brown);
        assert_eq!(config.pieces, PieceSet::Cburnett);
        assert_eq!(Config::from_toml("").unwrap(), config);
        assert_eq!(Config::from_toml("[theme]\n").unwrap(), config);
    }

    #[test]
    fn valid_toml_sets_both_theme_keys() {
        let config = Config::from_toml(
            r#"
            [theme]
            board = "dark"   # neutral
            pieces = "merida"
            "#,
        )
        .unwrap();
        assert_eq!(config.board, BoardTheme::Dark);
        assert_eq!(config.pieces, PieceSet::Merida);
    }

    #[test]
    fn every_documented_name_parses() {
        for theme in BoardTheme::ALL {
            let text = format!("[theme]\nboard = \"{}\"\n", theme.name());
            assert_eq!(Config::from_toml(&text).unwrap().board, theme);
        }
        for set in PieceSet::ALL {
            let text = format!("[theme]\npieces = \"{}\"\n", set.name());
            assert_eq!(Config::from_toml(&text).unwrap().pieces, set);
        }
    }

    #[test]
    fn partial_theme_keeps_the_other_default() {
        let config = Config::from_toml("[theme]\npieces = \"alpha\"\n").unwrap();
        assert_eq!(config.board, BoardTheme::Brown);
        assert_eq!(config.pieces, PieceSet::Alpha);
    }

    #[test]
    fn malformed_toml_is_an_error() {
        let err = Config::from_toml("[theme\nboard = ").unwrap_err();
        assert!(err.to_string().contains("invalid config.toml"), "{err}");
    }

    #[test]
    fn unknown_names_are_errors() {
        let board = Config::from_toml("[theme]\nboard = \"purple\"\n").unwrap_err();
        assert!(board.to_string().contains("unknown board theme"), "{board}");
        let pieces = Config::from_toml("[theme]\npieces = \"nostalgic\"\n").unwrap_err();
        assert!(pieces.to_string().contains("unknown piece set"), "{pieces}");
    }

    #[test]
    fn a_missing_file_is_not_an_error() {
        let path = std::env::temp_dir().join("lichess-rs-does-not-exist/config.toml");
        assert_eq!(Config::load(&path).unwrap(), None);
    }

    #[test]
    fn a_malformed_file_reports_its_path() {
        let dir = std::env::temp_dir().join(format!("lichess-rs-config-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, "board = [").unwrap();
        let err = Config::load(&path).unwrap_err();
        assert!(err.to_string().contains("invalid config.toml"), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn xdg_rules_cover_home_and_absolute_overrides() {
        assert_eq!(
            config_dir_from(Some("/tmp/xdg"), Some("/home/u")),
            Some(PathBuf::from("/tmp/xdg/lichess-rs"))
        );
        assert_eq!(
            config_dir_from(None, Some("/home/u")),
            Some(PathBuf::from("/home/u/.config/lichess-rs"))
        );
        // An empty XDG var is treated as unset, like the XDG spec says.
        assert_eq!(
            config_dir_from(Some(""), Some("/home/u")),
            Some(PathBuf::from("/home/u/.config/lichess-rs"))
        );
        assert_eq!(config_dir_from(None, None), None);
    }
}
