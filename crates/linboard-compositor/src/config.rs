//! Configuration file handling.
//!
//! Linboard reads a TOML file from `$XDG_CONFIG_HOME/linboard/config.toml` (or
//! `~/.config/linboard/config.toml`). Every field is optional; a missing file
//! yields [`Config::default`], which is a perfectly usable single-display setup.

use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub general: General,
    pub input: Input,
    /// Per-output overrides, matched by connector name (`DP-1`, `HDMI-A-1`, ...).
    #[serde(rename = "output")]
    pub outputs: Vec<OutputConfig>,
    /// `"Super+Q" = "close"` style bindings.
    pub keybindings: std::collections::HashMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct General {
    /// Commands spawned once the compositor is up and the socket is exported.
    pub autostart: Vec<String>,
    /// The session shell started by `--shell`, and whose exit ends the
    /// session. Only consulted when that flag is given.
    pub shell: String,
    /// How outputs with no explicit `position` are placed relative to each other.
    pub output_layout: OutputLayout,
    /// Gap, in logical pixels, inserted between auto-placed outputs.
    pub output_gap: i32,
    /// Background colour behind everything, as `[r, g, b, a]` in 0.0..=1.0.
    pub background: [f32; 4],
    /// Draw the compositor's own cursor. Disable when a nested parent already
    /// draws one for you.
    pub draw_cursor: bool,
    /// XCursor theme for the pointer. Defaults to the bundled Bibata Modern
    /// Classic, honouring an inherited `XCURSOR_THEME` before falling back.
    pub cursor_theme: Option<String>,
    /// Nominal cursor size in logical pixels; `XCURSOR_SIZE`, then 24.
    pub cursor_size: Option<u32>,
    /// Environment variables exported to every child process.
    pub env: std::collections::HashMap<String, String>,
}

impl Default for General {
    fn default() -> Self {
        Self {
            autostart: Vec::new(),
            shell: "linboard-xmb".into(),
            output_layout: OutputLayout::default(),
            output_gap: 0,
            background: [0.02, 0.02, 0.04, 1.0],
            draw_cursor: true,
            cursor_theme: None,
            cursor_size: None,
            env: std::collections::HashMap::new(),
        }
    }
}

/// Arrangement policy for outputs that do not pin themselves with `position`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OutputLayout {
    /// Left to right, in the order outputs appear.
    #[default]
    Horizontal,
    /// Top to bottom.
    Vertical,
    /// Every output shares the origin, showing the same region.
    Mirror,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Input {
    pub keyboard_layout: String,
    pub keyboard_variant: String,
    pub keyboard_options: Option<String>,
    pub keyboard_model: String,
    pub keyboard_rules: String,
    /// Key repeat rate in keys per second.
    pub repeat_rate: i32,
    /// Delay before key repeat kicks in, in milliseconds.
    pub repeat_delay: i32,
    pub tap_to_click: bool,
    pub natural_scroll: bool,
    pub disable_while_typing: bool,
    /// Pointer acceleration in -1.0..=1.0, libinput semantics.
    pub pointer_accel: f64,
}

impl Default for Input {
    fn default() -> Self {
        Self {
            keyboard_layout: "us".into(),
            keyboard_variant: String::new(),
            keyboard_options: None,
            keyboard_model: String::new(),
            keyboard_rules: String::new(),
            repeat_rate: 25,
            repeat_delay: 600,
            tap_to_click: true,
            natural_scroll: false,
            disable_while_typing: true,
            pointer_accel: 0.0,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OutputConfig {
    /// Connector name. `"*"` matches any output that has no more specific entry.
    pub name: String,
    /// `"1920x1080@60"`, `"1920x1080"`, or `"preferred"`.
    pub mode: Option<String>,
    /// Explicit logical position; when absent the output is auto-placed.
    pub position: Option<[i32; 2]>,
    pub scale: Option<f64>,
    /// `normal`, `90`, `180`, `270`, `flipped`, `flipped-90`, ...
    pub transform: Option<String>,
    /// Disabled outputs are left unlit and take part in no layout.
    pub enabled: Option<bool>,
    /// Force variable refresh rate on this output when the hardware allows it.
    pub adaptive_sync: Option<bool>,
}

/// A parsed `WIDTHxHEIGHT@REFRESH` mode request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModeRequest {
    pub width: i32,
    pub height: i32,
    /// Refresh in mHz, as DRM reports it. `None` means "any refresh rate".
    pub refresh: Option<i32>,
}

impl OutputConfig {
    /// Parse [`OutputConfig::mode`]. Returns `None` for `preferred`/absent modes.
    pub fn parse_mode(&self) -> Option<ModeRequest> {
        let raw = self.mode.as_deref()?;
        if raw.eq_ignore_ascii_case("preferred") {
            return None;
        }
        let (dims, refresh) = match raw.split_once('@') {
            Some((d, r)) => (d, Some(r)),
            None => (raw, None),
        };
        let (w, h) = dims.split_once('x')?;
        let refresh = refresh.and_then(|r| {
            let r = r.trim().trim_end_matches("Hz").trim();
            // Accept both `60` and `59.951`; DRM works in mHz.
            r.parse::<f64>().ok().map(|hz| (hz * 1000.0).round() as i32)
        });
        Some(ModeRequest {
            width: w.trim().parse().ok()?,
            height: h.trim().parse().ok()?,
            refresh,
        })
    }
}

impl Config {
    /// Default path, honouring `XDG_CONFIG_HOME`.
    pub fn default_path() -> Option<PathBuf> {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
        Some(base.join("linboard").join("config.toml"))
    }

    /// Load from `path`. A missing file is not an error.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(raw) => {
                let cfg: Config = toml::from_str(&raw)?;
                tracing::info!(path = %path.display(), "loaded config");
                Ok(cfg)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::debug!(path = %path.display(), "no config file, using defaults");
                Ok(Config::default())
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Resolve the config entry for a connector, preferring an exact name match
    /// over the `"*"` wildcard.
    pub fn output_for(&self, name: &str) -> Option<&OutputConfig> {
        self.outputs
            .iter()
            .find(|o| o.name == name)
            .or_else(|| self.outputs.iter().find(|o| o.name == "*"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_modes() {
        let with_refresh = OutputConfig {
            mode: Some("2560x1440@144".into()),
            ..Default::default()
        };
        assert_eq!(
            with_refresh.parse_mode(),
            Some(ModeRequest {
                width: 2560,
                height: 1440,
                refresh: Some(144_000)
            })
        );

        let no_refresh = OutputConfig {
            mode: Some("1920x1080".into()),
            ..Default::default()
        };
        assert_eq!(
            no_refresh.parse_mode(),
            Some(ModeRequest {
                width: 1920,
                height: 1080,
                refresh: None
            })
        );

        let preferred = OutputConfig {
            mode: Some("preferred".into()),
            ..Default::default()
        };
        assert_eq!(preferred.parse_mode(), None);

        let fractional = OutputConfig {
            mode: Some("1920x1080@59.951".into()),
            ..Default::default()
        };
        assert_eq!(fractional.parse_mode().unwrap().refresh, Some(59_951));
    }

    #[test]
    fn exact_output_match_beats_wildcard() {
        let cfg: Config = toml::from_str(
            r#"
            [[output]]
            name = "*"
            scale = 1.0

            [[output]]
            name = "DP-1"
            scale = 2.0
            "#,
        )
        .unwrap();
        assert_eq!(cfg.output_for("DP-1").unwrap().scale, Some(2.0));
        assert_eq!(cfg.output_for("HDMI-A-1").unwrap().scale, Some(1.0));
    }
}
