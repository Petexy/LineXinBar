//! What the displays were last set to, so the next session comes up in it.
//!
//! A compositor that lights a display before it knows how that display should
//! be driven pays a modeset for every setting it learns afterwards, and a
//! modeset is a black screen: the pipe goes down, the panel re-locks, and on
//! the hardware this was measured on that is 169 ms of nothing followed by
//! another 196 ms a second later. The settings arrive over `lxb_shell_v1`, and
//! the shell cannot send them until it has a Wayland connection — by which
//! time the displays have been up for a second in whatever the compositor
//! guessed.
//!
//! So the compositor writes down what it was asked for, and reads it back
//! before it lights anything. The second login onwards, the first commit
//! already carries the right colour pipeline and the driver has nothing to
//! change.
//!
//! This is state, not configuration: it lives under `XDG_STATE_HOME` and it is
//! written by the machine rather than by a person. Where the config file names
//! the same key it wins, because that one *was* written by a person — see
//! [`crate::config::Config::remember_displays`].

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::config::OutputConfig;

/// The file, honouring `XDG_STATE_HOME`.
///
/// `~/.local/state` rather than `~/.config`: the basedir spec keeps "state that
/// should persist but is not important enough for the config directory" here,
/// and remembered display settings are exactly that. A user who deletes it
/// loses one seamless login and nothing else.
pub fn path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("state"))
        })?;
    Some(base.join("lxb").join("displays.toml"))
}

/// The same `[[output]]` shape the config file uses, so the two merge without
/// translation and so the file can be read by a person who finds it.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Remembered {
    #[serde(rename = "output", default, skip_serializing_if = "Vec::is_empty")]
    outputs: Vec<OutputConfig>,
}

fn read() -> Remembered {
    let Some(path) = path() else {
        return Remembered::default();
    };
    match std::fs::read_to_string(&path) {
        Ok(raw) => match toml::from_str(&raw) {
            Ok(remembered) => remembered,
            Err(err) => {
                // Not fatal, and deliberately not repaired: a file this
                // compositor cannot read is one a later version wrote, or one
                // somebody edited. Coming up on the defaults costs a modeset;
                // deleting what we failed to understand could cost a display
                // configuration.
                tracing::warn!(path = %path.display(), %err, "cannot read what the displays were last set to");
                Remembered::default()
            }
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Remembered::default(),
        Err(err) => {
            tracing::warn!(path = %path.display(), %err, "cannot open what the displays were last set to");
            Remembered::default()
        }
    }
}

/// What every display was last set to, for [`crate::config::Config::remember_displays`].
pub fn load() -> Vec<OutputConfig> {
    let outputs = read().outputs;
    if !outputs.is_empty() {
        tracing::info!(
            displays = outputs.len(),
            "bringing the displays up the way they were left"
        );
    }
    outputs
}

/// Write down a change to one display, leaving every other display's entry and
/// every key of this one that `change` does not touch exactly as they were.
///
/// Failure is reported and never propagated. Not being able to write this file
/// makes the *next* login blink; making it fatal would end the session the user
/// is in, which is very much worse.
pub fn remember(name: &str, change: impl FnOnce(&mut OutputConfig)) {
    let Some(path) = path() else {
        return;
    };
    let mut remembered = read();
    match remembered.outputs.iter_mut().find(|o| o.name == name) {
        Some(entry) => change(entry),
        None => {
            let mut entry = OutputConfig {
                name: name.to_owned(),
                ..OutputConfig::default()
            };
            change(&mut entry);
            remembered.outputs.push(entry);
        }
    }

    let body = match toml::to_string_pretty(&remembered) {
        Ok(body) => body,
        Err(err) => {
            tracing::warn!(%err, "cannot write down what this display is set to");
            return;
        }
    };

    if let Some(parent) = path.parent() {
        if let Err(err) = std::fs::create_dir_all(parent) {
            tracing::warn!(path = %parent.display(), %err, "cannot make the directory the display settings live in");
            return;
        }
    }

    // Written beside the file and renamed over it, so a compositor that is
    // killed mid-write leaves the previous answer rather than half of this one.
    // `rename` within a directory is atomic, which `write` is not.
    let scratch = path.with_extension("toml.new");
    if let Err(err) = std::fs::write(&scratch, body) {
        tracing::warn!(path = %scratch.display(), %err, "cannot write down what this display is set to");
        return;
    }
    if let Err(err) = std::fs::rename(&scratch, &path) {
        tracing::warn!(path = %path.display(), %err, "cannot write down what this display is set to");
        let _ = std::fs::remove_file(&scratch);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The round trip has to survive the config file's own parser, because that
    /// is what reads this file back: `deny_unknown_fields` means a key spelled
    /// differently on the way out than on the way in is not a cosmetic bug, it
    /// is a file the next session refuses.
    #[test]
    fn what_is_written_is_what_the_config_parser_reads() {
        let remembered = Remembered {
            outputs: vec![OutputConfig {
                name: "DP-2".into(),
                mode: Some("2560x1440@239.97".into()),
                hdr: Some(true),
                hdr_sdr_brightness: Some(250),
                hdr_srgb_intensity: Some(0),
                hdr_peak_brightness: Some(0),
                night_light: Some(true),
                night_light_temperature: Some(3600),
                ..OutputConfig::default()
            }],
        };

        let body = toml::to_string_pretty(&remembered).expect("writes");
        let back: Remembered = toml::from_str(&body).expect("reads back");

        assert_eq!(back.outputs.len(), 1);
        let entry = &back.outputs[0];
        assert_eq!(entry.name, "DP-2");
        assert_eq!(entry.hdr, Some(true));
        assert_eq!(entry.hdr_sdr_brightness, Some(250));
        assert_eq!(entry.night_light_temperature, Some(3600));
        assert_eq!(entry.parse_mode().expect("a mode").refresh, Some(239_970));
    }

    /// Nothing a display was never asked about is written, so a file for one
    /// display that only ever had its night light changed does not also assert
    /// a resolution nobody chose.
    #[test]
    fn only_what_was_asked_for_is_written() {
        let remembered = Remembered {
            outputs: vec![OutputConfig {
                name: "HDMI-A-1".into(),
                night_light: Some(true),
                night_light_temperature: Some(4000),
                ..OutputConfig::default()
            }],
        };

        let body = toml::to_string_pretty(&remembered).expect("writes");
        assert!(body.contains("night_light"), "{body}");
        assert!(!body.contains("mode"), "{body}");
        assert!(!body.contains("hdr"), "{body}");
        assert!(!body.contains("scale"), "{body}");
    }
}
