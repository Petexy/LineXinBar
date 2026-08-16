//! Configuration file handling.
//!
//! LineXinBar reads a TOML file from `$XDG_CONFIG_HOME/lxb/config.toml` (or
//! `~/.config/lxb/config.toml`). Every field is optional; a missing file
//! yields [`Config::default`], which is a perfectly usable single-display setup.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

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
            shell: "lxb-desktop".into(),
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

/// One display's settings, as the config file spells them — and as
/// [`crate::remembered`] writes them back down, which is why every optional key
/// is skipped when it is absent rather than written as a null: the two files
/// share this shape and the config parser refuses keys it does not know.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct OutputConfig {
    /// Connector name. `"*"` matches any output that has no more specific entry.
    pub name: String,
    /// `"1920x1080@60"`, `"1920x1080"`, or `"preferred"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// Explicit logical position; when absent the output is auto-placed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<[i32; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scale: Option<f64>,
    /// `normal`, `90`, `180`, `270`, `flipped`, `flipped-90`, ...
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transform: Option<String>,
    /// Disabled outputs are left unlit and take part in no layout.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Force variable refresh rate on this output when the hardware allows it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adaptive_sync: Option<bool>,
    /// Drive this output in high dynamic range when the display and the driver
    /// both allow it. The shell's Settings column writes the same thing at
    /// runtime; this is what the session comes up in.
    ///
    /// The one key here that costs a modeset to change, which is why it is also
    /// the one that most needs to be right before the first commit: turning HDR
    /// on or off re-drives the pipe and the panel re-locks behind it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hdr: Option<bool>,
    /// Luminance plain white is sent at while HDR is on, in cd/m².
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hdr_sdr_brightness: Option<u16>,
    /// How far sRGB colour is stretched towards BT.2020, 0 to 100.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hdr_srgb_intensity: Option<u8>,
    /// Peak luminance declared to the display, in cd/m². Absent means "use
    /// whatever the display says about itself".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hdr_peak_brightness: Option<u16>,
    /// Warm this display's picture, and how far — in kelvin, lower being
    /// warmer.
    ///
    /// What the session **comes up in**, as the four `hdr_*` keys are. There is
    /// deliberately no schedule here to go with them: keeping hours means a
    /// clock and a time zone, and the compositor owns neither — the shell works
    /// out whether the light should be burning and sends the answer. So this is
    /// the setting for a session with no shell, or for what a display is warmed
    /// to until one connects.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub night_light: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub night_light_temperature: Option<u16>,
}

/// A parsed `WIDTHxHEIGHT@REFRESH` mode request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModeRequest {
    pub width: i32,
    pub height: i32,
    /// Refresh in mHz, as DRM reports it. `None` means "any refresh rate".
    pub refresh: Option<i32>,
}

impl ModeRequest {
    /// How the config file spells this mode, so one chosen at runtime can be
    /// written back down and parsed again by [`OutputConfig::parse_mode`].
    pub fn as_config_string(&self) -> String {
        match self.refresh {
            // DRM works in mHz and the file is in Hz. The trailing zeros of a
            // whole rate are trimmed, so 60000 comes back as `@60` rather than
            // `@60.000` — both parse, but only one is what a person would have
            // written in the file beside it.
            Some(mhz) => {
                let mut rate = format!("{:.3}", f64::from(mhz) / 1000.0);
                while rate.ends_with('0') {
                    rate.pop();
                }
                if rate.ends_with('.') {
                    rate.pop();
                }
                format!("{}x{}@{}", self.width, self.height, rate)
            }
            None => format!("{}x{}", self.width, self.height),
        }
    }
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

    /// The HDR pipeline this entry describes, with the compositor's own
    /// defaults standing in for everything it leaves out.
    pub fn hdr_settings(&self) -> crate::hdr::Settings {
        let defaults = crate::hdr::Settings::default();
        crate::hdr::Settings {
            enabled: self.hdr.unwrap_or(defaults.enabled),
            sdr_brightness: self.hdr_sdr_brightness.unwrap_or(defaults.sdr_brightness),
            srgb_intensity: self
                .hdr_srgb_intensity
                .unwrap_or(defaults.srgb_intensity)
                .min(100),
            peak_brightness: self.hdr_peak_brightness.or(defaults.peak_brightness),
        }
    }

    /// Take from `other` every key this entry does not already state.
    ///
    /// Per key rather than per display: a config that pins one display's
    /// resolution and says nothing about its colour should keep the resolution
    /// *and* come up in the HDR it was left in, which an all-or-nothing merge
    /// would not give it.
    fn fill_from(&mut self, other: OutputConfig) {
        let OutputConfig {
            name: _,
            mode,
            position,
            scale,
            transform,
            enabled,
            adaptive_sync,
            hdr,
            hdr_sdr_brightness,
            hdr_srgb_intensity,
            hdr_peak_brightness,
            night_light,
            night_light_temperature,
        } = other;
        // Destructured rather than field-by-field so that a key added to
        // `OutputConfig` and forgotten here fails to compile instead of
        // silently never being remembered.
        self.mode = self.mode.take().or(mode);
        self.position = self.position.or(position);
        self.scale = self.scale.or(scale);
        self.transform = self.transform.take().or(transform);
        self.enabled = self.enabled.or(enabled);
        self.adaptive_sync = self.adaptive_sync.or(adaptive_sync);
        self.hdr = self.hdr.or(hdr);
        self.hdr_sdr_brightness = self.hdr_sdr_brightness.or(hdr_sdr_brightness);
        self.hdr_srgb_intensity = self.hdr_srgb_intensity.or(hdr_srgb_intensity);
        self.hdr_peak_brightness = self.hdr_peak_brightness.or(hdr_peak_brightness);
        self.night_light = self.night_light.or(night_light);
        self.night_light_temperature = self.night_light_temperature.or(night_light_temperature);
    }

    /// The night light this entry describes, clamped to what the ramp can
    /// encode — a hand-written temperature out of range lands on the nearest
    /// end rather than taking the config down with it.
    pub fn night_light(&self) -> crate::hdr::NightLight {
        let defaults = crate::hdr::NightLight::default();
        crate::hdr::NightLight {
            enabled: self.night_light.unwrap_or(defaults.enabled),
            temperature: self
                .night_light_temperature
                .unwrap_or(defaults.temperature)
                .clamp(crate::hdr::WARMEST_KELVIN, crate::hdr::NEUTRAL_KELVIN),
        }
    }
}

impl Config {
    /// The HDR settings a connector should come up in, or the defaults when
    /// the config says nothing about it.
    pub fn hdr_for(&self, name: &str) -> crate::hdr::Settings {
        self.output_for(name)
            .map(OutputConfig::hdr_settings)
            .unwrap_or_default()
    }

    /// The same for the night light: what a connector comes up warmed to,
    /// which for a config that says nothing is not warmed at all.
    pub fn night_light_for(&self, name: &str) -> crate::hdr::NightLight {
        self.output_for(name)
            .map(OutputConfig::night_light)
            .unwrap_or_default()
    }

    /// Fold what the displays were last set to in underneath this config, so
    /// the first commit already carries the colour pipeline the session ends up
    /// wanting instead of reaching it a second later through a second modeset.
    ///
    /// Underneath, not over: this config file was written by a person and the
    /// remembered state was written by the machine, so a key spelled out here
    /// wins and remembered state only fills what this one leaves unsaid. A
    /// machine whose config pins `hdr = false` stays in SDR however many times
    /// the shell is asked for HDR, which is what pinning it meant.
    pub fn remember_displays(&mut self, remembered: Vec<OutputConfig>) {
        for entry in remembered {
            match self.outputs.iter_mut().find(|o| o.name == entry.name) {
                Some(existing) => existing.fill_from(entry),
                None => self.outputs.push(entry),
            }
        }
    }
}

impl Config {
    /// Default path, honouring `XDG_CONFIG_HOME`.
    pub fn default_path() -> Option<PathBuf> {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
        Some(base.join("lxb").join("config.toml"))
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

    /// A mode chosen at runtime is written into the same file the config is
    /// read from, so the spelling has to survive the trip. The rate that
    /// matters here is the awkward one: DP-2 runs at 239970 mHz, and a
    /// compositor that wrote that back as `@239` or `@240` would come up on a
    /// mode the connector does not list and fall back to the preferred one.
    #[test]
    fn a_mode_survives_being_written_down_and_read_back() {
        for original in [
            ModeRequest {
                width: 2560,
                height: 1440,
                refresh: Some(239_970),
            },
            ModeRequest {
                width: 2560,
                height: 1440,
                refresh: Some(59_951),
            },
            ModeRequest {
                width: 1920,
                height: 1080,
                refresh: Some(60_000),
            },
            ModeRequest {
                width: 1280,
                height: 720,
                refresh: None,
            },
        ] {
            let spelled = original.as_config_string();
            let back = OutputConfig {
                mode: Some(spelled.clone()),
                ..Default::default()
            }
            .parse_mode()
            .unwrap_or_else(|| panic!("{spelled} does not parse"));
            assert_eq!(back, original, "{spelled}");
        }

        // A whole rate keeps a person's spelling rather than `@60.000`.
        assert_eq!(
            ModeRequest {
                width: 1920,
                height: 1080,
                refresh: Some(60_000)
            }
            .as_config_string(),
            "1920x1080@60"
        );
    }

    /// The config file was written by a person and the remembered state was
    /// written by the machine, so the person wins — but only key by key, so a
    /// config that pins a resolution still inherits the colour it was left in.
    #[test]
    fn a_written_config_outranks_what_was_remembered() {
        let mut config = Config {
            outputs: vec![OutputConfig {
                name: "DP-2".into(),
                mode: Some("1920x1080@60".into()),
                hdr: Some(false),
                ..Default::default()
            }],
            ..Default::default()
        };

        config.remember_displays(vec![
            OutputConfig {
                name: "DP-2".into(),
                mode: Some("2560x1440@239.97".into()),
                hdr: Some(true),
                night_light: Some(true),
                night_light_temperature: Some(3600),
                ..Default::default()
            },
            OutputConfig {
                name: "HDMI-A-1".into(),
                night_light: Some(true),
                night_light_temperature: Some(4000),
                ..Default::default()
            },
        ]);

        // Pinned by hand, so remembering something else changes nothing.
        assert!(!config.hdr_for("DP-2").enabled);
        assert_eq!(
            config.output_for("DP-2").and_then(|o| o.mode.clone()),
            Some("1920x1080@60".into())
        );
        // Left unsaid by hand, so what the display was last set to fills it.
        assert_eq!(config.night_light_for("DP-2").temperature, 3600);
        // Never mentioned by hand at all, so the whole entry arrives.
        assert_eq!(config.night_light_for("HDMI-A-1").temperature, 4000);
        assert!(config.night_light_for("HDMI-A-1").enabled);
    }

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

    /// What a display comes up warmed to, and what a hand-written temperature
    /// out of range comes to.
    ///
    /// A config file is something the user opens in an editor, so a silly
    /// number in it must land on the nearest picture that means something
    /// rather than take the display — or the rest of the file — with it.
    #[test]
    fn a_display_comes_up_warmed_only_if_the_config_says_so() {
        let cfg: Config = toml::from_str(
            r#"
            [[output]]
            name = "TEST-OUT-1"
            night_light = true
            night_light_temperature = 2700

            [[output]]
            name = "TEST-OUT-2"
            night_light = true
            night_light_temperature = 100
            "#,
        )
        .unwrap();

        let warm = cfg.night_light_for("TEST-OUT-1");
        assert!(warm.enabled);
        assert_eq!(warm.temperature, 2700);

        // Clamped to the warm end rather than refused.
        let silly = cfg.night_light_for("TEST-OUT-2");
        assert!(silly.enabled);
        assert_eq!(silly.temperature, crate::hdr::WARMEST_KELVIN);

        // A display the file says nothing about is not warmed, and neither is
        // one in a file that says nothing at all: there is no session-wide
        // default standing behind this.
        assert_eq!(
            cfg.night_light_for("TEST-OUT-3"),
            crate::hdr::NightLight::default()
        );
        assert!(!Config::default().night_light_for("TEST-OUT-1").enabled);
    }

    #[test]
    fn exact_output_match_beats_wildcard() {
        let cfg: Config = toml::from_str(
            r#"
            [[output]]
            name = "*"
            scale = 1.0

            [[output]]
            name = "TEST-OUT-1"
            scale = 2.0
            "#,
        )
        .unwrap();
        assert_eq!(cfg.output_for("TEST-OUT-1").unwrap().scale, Some(2.0));
        assert_eq!(cfg.output_for("TEST-OUT-2").unwrap().scale, Some(1.0));
    }
}
