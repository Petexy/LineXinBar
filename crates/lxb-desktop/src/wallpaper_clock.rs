//! The one piece of the login screen that survives into the shell: where the
//! analytic wallpaper's clock had reached.
//!
//! A display manager cannot hand an `Instant` to another process. It can hand
//! over one sample from Linux's monotonic clock and the wallpaper scene time
//! at that sample. Advancing both from the same clock lets the first shell
//! frame carry on from the login screen instead of beginning at zero.

use std::{fmt, fs, time::Instant};

/// Public because the producer and consumer must agree on this spelling.
pub const HANDOFF_ENV: &str = "LXB_BACKGROUND_HANDOFF";

/// The scene both ends of a handoff have to be drawing. Named where the scene
/// is — `lxb-protocol` — so the compositor's bridge frame and this shell cannot
/// disagree about which wallpaper a record is for.
const VISUAL_ID: &str = lxb_protocol::wallpaper::VISUAL;
const CLOCK_ID: &str = "linux-monotonic";
const MAX_RECORD_BYTES: usize = 1024;
const MAX_HANDOFF_AGE_NS: u64 = 30_000_000_000;
const NANOS_PER_SECOND: u64 = 1_000_000_000;
const BOOT_ID_PATH: &str = "/proc/sys/kernel/random/boot_id";

/// A clock used only by the wallpaper shader.
///
/// `Shell::start` deliberately remains separate: controller motion, pointer
/// integration, debug actions, and the on-screen keyboard all use that local
/// process clock and must not inherit time from the display manager.
pub struct WallpaperClock {
    started: Instant,
    scene_ns_at_start: u64,
}

impl WallpaperClock {
    /// Consume a valid desktop-manager handoff.
    ///
    /// This is called by `main` before it starts any worker threads. Removing
    /// the variable here keeps this shell from passing the one-shot record to
    /// children of its own. `None` leaves `main` to create the local clock at
    /// the same late point where `Shell::start` has always begun.
    pub fn from_environment(configured_accent: &str) -> Option<Self> {
        let raw = std::env::var_os(HANDOFF_ENV)?;

        // SAFETY: `main` calls this before starting the media, Steam, artwork,
        // thumbnail, notification, or policy-agent workers, so no other thread
        // in this process can be reading the environment concurrently.
        unsafe { std::env::remove_var(HANDOFF_ENV) };

        let accepted = raw
            .to_str()
            .ok_or(Rejection::NonUtf8)
            .and_then(parse)
            .and_then(|handoff| {
                let boot_id = current_boot_id()?;
                let now_ns = monotonic_now_ns()?;
                let scene_ns = scene_at(&handoff, &boot_id, now_ns)?;
                Ok((scene_ns, handoff.accent))
            });

        match accepted {
            Ok((scene_ns, handoff_accent)) => {
                if handoff_accent != configured_accent {
                    // The saved shell setting remains authoritative. Keeping
                    // the valid phase still avoids adding a second jump to the
                    // unavoidable colour correction.
                    tracing::warn!(
                        handoff_accent,
                        configured_accent,
                        "background handoff used a different accent; keeping the shell setting"
                    );
                }
                tracing::info!(visual = VISUAL_ID, "continuing login background animation");
                Some(Self {
                    started: Instant::now(),
                    scene_ns_at_start: scene_ns,
                })
            }
            Err(reason) => {
                tracing::warn!(%reason, "ignoring background handoff");
                None
            }
        }
    }

    pub fn local() -> Self {
        Self {
            started: Instant::now(),
            scene_ns_at_start: 0,
        }
    }

    /// The shader still consumes an `f32`; retain nanosecond precision until
    /// this boundary so session-start latency is not rounded repeatedly.
    pub fn elapsed_secs(&self) -> f32 {
        let elapsed_ns = u64::try_from(self.started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        let scene_ns = self.scene_ns_at_start.saturating_add(elapsed_ns);
        (scene_ns as f64 / NANOS_PER_SECOND as f64) as f32
    }
}

#[derive(Debug)]
struct Handoff {
    boot_id: String,
    sample_ns: u64,
    scene_ns: u64,
    accent: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rejection {
    TooLong,
    NonAscii,
    NonUtf8,
    MalformedField,
    UnknownField,
    DuplicateField,
    MissingField,
    UnsupportedVersion,
    UnsupportedVisual,
    UnsupportedClock,
    InvalidBootId,
    InvalidSample,
    InvalidScene,
    InvalidAccent,
    BootChanged,
    FutureSample,
    Expired,
    Overflow,
    BootIdUnavailable,
    MonotonicClockUnavailable,
}

impl fmt::Display for Rejection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::TooLong => "record is too long",
            Self::NonAscii => "record is not ASCII",
            Self::NonUtf8 => "record is not UTF-8",
            Self::MalformedField => "record has a malformed field",
            Self::UnknownField => "record has an unknown field",
            Self::DuplicateField => "record repeats a field",
            Self::MissingField => "record is missing a field",
            Self::UnsupportedVersion => "record version is unsupported",
            Self::UnsupportedVisual => "wallpaper visual version is unsupported",
            Self::UnsupportedClock => "record clock is unsupported",
            Self::InvalidBootId => "boot ID is malformed",
            Self::InvalidSample => "monotonic sample is malformed",
            Self::InvalidScene => "scene time is malformed",
            Self::InvalidAccent => "accent name is not canonical",
            Self::BootChanged => "record came from another boot",
            Self::FutureSample => "monotonic sample is in the future",
            Self::Expired => "record is stale",
            Self::Overflow => "scene time overflowed",
            Self::BootIdUnavailable => "current boot ID is unavailable",
            Self::MonotonicClockUnavailable => "monotonic clock is unavailable",
        })
    }
}

fn parse(record: &str) -> Result<Handoff, Rejection> {
    if record.len() > MAX_RECORD_BYTES {
        return Err(Rejection::TooLong);
    }
    if !record.is_ascii() {
        return Err(Rejection::NonAscii);
    }

    let mut version = None;
    let mut visual = None;
    let mut clock = None;
    let mut boot_id = None;
    let mut sample_ns = None;
    let mut scene_ns = None;
    let mut accent = None;
    // Accepted and then dropped on the floor, deliberately. The display manager
    // writes the material the *wallpaper* was drawn in for the benefit of a
    // reader that cannot see this account's settings — the compositor drawing a
    // bridge frame in front of the *login screen* runs as the greeter's own
    // account. This shell is the account, has already read `shell.toml`, and a
    // record that disagreed with it would be a second opinion about a setting
    // with a page in front of it. What is not acceptable is refusing the record
    // over a field that is none of this module's business, because that would
    // throw the phase away with it.
    //
    // One field although the Theme setting is two: the marks the shell draws are
    // no part of a wallpaper, so the reader this is written for has no use for
    // them and the record has never carried them.
    let mut theme = None;

    for field in record.split(';') {
        let (key, value) = field.split_once('=').ok_or(Rejection::MalformedField)?;
        if key.is_empty() || value.is_empty() {
            return Err(Rejection::MalformedField);
        }

        let slot = match key {
            "v" => &mut version,
            "visual" => &mut visual,
            "clock" => &mut clock,
            "boot" => &mut boot_id,
            "sample-ns" => &mut sample_ns,
            "scene-ns" => &mut scene_ns,
            "accent" => &mut accent,
            "theme" => &mut theme,
            _ => return Err(Rejection::UnknownField),
        };
        if slot.replace(value).is_some() {
            return Err(Rejection::DuplicateField);
        }
    }

    let version = version.ok_or(Rejection::MissingField)?;
    let visual = visual.ok_or(Rejection::MissingField)?;
    let clock = clock.ok_or(Rejection::MissingField)?;
    let boot_id = boot_id.ok_or(Rejection::MissingField)?;
    let sample_ns = sample_ns.ok_or(Rejection::MissingField)?;
    let scene_ns = scene_ns.ok_or(Rejection::MissingField)?;
    let accent = accent.ok_or(Rejection::MissingField)?;

    if version != "1" {
        return Err(Rejection::UnsupportedVersion);
    }
    if visual != VISUAL_ID {
        return Err(Rejection::UnsupportedVisual);
    }
    if clock != CLOCK_ID {
        return Err(Rejection::UnsupportedClock);
    }
    if !valid_boot_id(boot_id) {
        return Err(Rejection::InvalidBootId);
    }
    // Asked of the shared table rather than of a list spelled out here: the
    // accent names have enough homes already, and one more is one more place
    // for them to drift apart.
    if !lxb_protocol::wallpaper::PALETTES
        .iter()
        .any(|palette| palette.name == accent)
    {
        return Err(Rejection::InvalidAccent);
    }

    Ok(Handoff {
        boot_id: boot_id.to_string(),
        sample_ns: parse_decimal(sample_ns).ok_or(Rejection::InvalidSample)?,
        scene_ns: parse_decimal(scene_ns).ok_or(Rejection::InvalidScene)?,
        accent: accent.to_string(),
    })
}

fn parse_decimal(value: &str) -> Option<u64> {
    value
        .bytes()
        .all(|byte| byte.is_ascii_digit())
        .then(|| value.parse().ok())
        .flatten()
}

fn scene_at(handoff: &Handoff, boot_id: &str, now_ns: u64) -> Result<u64, Rejection> {
    if handoff.boot_id != boot_id {
        return Err(Rejection::BootChanged);
    }
    let age_ns = now_ns
        .checked_sub(handoff.sample_ns)
        .ok_or(Rejection::FutureSample)?;
    if age_ns > MAX_HANDOFF_AGE_NS {
        return Err(Rejection::Expired);
    }
    handoff
        .scene_ns
        .checked_add(age_ns)
        .ok_or(Rejection::Overflow)
}

fn valid_boot_id(id: &str) -> bool {
    if id.len() != 36 {
        return false;
    }
    id.bytes().enumerate().all(|(index, byte)| {
        if matches!(index, 8 | 13 | 18 | 23) {
            byte == b'-'
        } else {
            byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
        }
    })
}

fn current_boot_id() -> Result<String, Rejection> {
    let id = fs::read_to_string(BOOT_ID_PATH).map_err(|_| Rejection::BootIdUnavailable)?;
    let id = id.trim();
    valid_boot_id(id)
        .then(|| id.to_string())
        .ok_or(Rejection::BootIdUnavailable)
}

fn monotonic_now_ns() -> Result<u64, Rejection> {
    let mut timestamp = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `timestamp` points to a live `timespec`, and CLOCK_MONOTONIC does
    // not require any additional lifetime or ownership guarantees.
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut timestamp) } != 0
        || timestamp.tv_sec < 0
        || !(0..NANOS_PER_SECOND as _).contains(&timestamp.tv_nsec)
    {
        return Err(Rejection::MonotonicClockUnavailable);
    }

    let seconds =
        u64::try_from(timestamp.tv_sec).map_err(|_| Rejection::MonotonicClockUnavailable)?;
    let nanos =
        u64::try_from(timestamp.tv_nsec).map_err(|_| Rejection::MonotonicClockUnavailable)?;
    seconds
        .checked_mul(NANOS_PER_SECOND)
        .and_then(|value| value.checked_add(nanos))
        .ok_or(Rejection::MonotonicClockUnavailable)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOOT_ID: &str = "01234567-89ab-cdef-0123-456789abcdef";

    fn valid_record() -> String {
        format!(
            "v=1;visual={VISUAL_ID};clock={CLOCK_ID};boot={BOOT_ID};sample-ns=10000000000;scene-ns=42000000000;accent=Blue"
        )
    }

    #[test]
    fn canonical_fixture_matches_the_display_manager_encoder() {
        assert_eq!(
            valid_record(),
            "v=1;visual=lxb-wallpaper-v2;clock=linux-monotonic;boot=01234567-89ab-cdef-0123-456789abcdef;sample-ns=10000000000;scene-ns=42000000000;accent=Blue"
        );
    }

    #[test]
    fn parses_the_complete_versioned_record() {
        let handoff = parse(&valid_record()).expect("valid handoff");
        assert_eq!(handoff.boot_id, BOOT_ID);
        assert_eq!(handoff.sample_ns, 10_000_000_000);
        assert_eq!(handoff.scene_ns, 42_000_000_000);
        assert_eq!(handoff.accent, "Blue");
    }

    #[test]
    fn field_order_does_not_change_the_record() {
        let handoff = parse(&format!(
            "accent=Red;scene-ns=7;sample-ns=3;boot={BOOT_ID};clock={CLOCK_ID};visual={VISUAL_ID};v=1"
        ))
        .expect("reordered handoff");
        assert_eq!(handoff.scene_ns, 7);
        assert_eq!(handoff.accent, "Red");
    }

    #[test]
    fn rejects_partial_duplicate_and_extended_records() {
        assert_eq!(parse("v=1").unwrap_err(), Rejection::MissingField);
        assert_eq!(
            parse(&format!("{};v=1", valid_record())).unwrap_err(),
            Rejection::DuplicateField
        );
        assert_eq!(
            parse(&format!("{};extra=1", valid_record())).unwrap_err(),
            Rejection::UnknownField
        );
    }

    #[test]
    fn rejects_incompatible_or_ambiguous_values() {
        assert_eq!(
            parse(&valid_record().replace("v=1", "v=2")).unwrap_err(),
            Rejection::UnsupportedVersion
        );
        // The retired identifier rather than an invented one, because that is
        // the case this field exists for: a display manager still on the older
        // wallpaper, handing its phase to a shell that would draw a different
        // scene at the same time. Continuing from it is worse than starting
        // again, and the record has to be refused for that to happen.
        assert_eq!(
            parse(&valid_record().replace(VISUAL_ID, "lxb-wallpaper-v1")).unwrap_err(),
            Rejection::UnsupportedVisual
        );
        assert_eq!(
            parse(&valid_record().replace(CLOCK_ID, "realtime")).unwrap_err(),
            Rejection::UnsupportedClock
        );
        assert_eq!(
            parse(&valid_record().replace("accent=Blue", "accent=blue")).unwrap_err(),
            Rejection::InvalidAccent
        );
        assert_eq!(
            parse(&valid_record().replace(BOOT_ID, "not-a-boot-id")).unwrap_err(),
            Rejection::InvalidBootId
        );
        assert_eq!(
            parse(&valid_record().replace("sample-ns=10000000000", "sample-ns=+10000000000"))
                .unwrap_err(),
            Rejection::InvalidSample
        );
    }

    #[test]
    fn rejects_non_ascii_and_oversized_records() {
        assert_eq!(
            parse("accent=Bl\u{00fa}e").unwrap_err(),
            Rejection::NonAscii
        );
        assert_eq!(
            parse(&"x".repeat(MAX_RECORD_BYTES + 1)).unwrap_err(),
            Rejection::TooLong
        );
    }

    #[test]
    fn advances_scene_time_by_monotonic_elapsed_time() {
        let handoff = parse(&valid_record()).expect("valid handoff");
        assert_eq!(
            scene_at(&handoff, BOOT_ID, 10_250_000_000).expect("fresh handoff"),
            42_250_000_000
        );
    }

    #[test]
    fn rejects_another_boot_future_samples_and_stale_samples() {
        let handoff = parse(&valid_record()).expect("valid handoff");
        assert_eq!(
            scene_at(
                &handoff,
                "ffffffff-ffff-ffff-ffff-ffffffffffff",
                10_000_000_000
            )
            .unwrap_err(),
            Rejection::BootChanged
        );
        assert_eq!(
            scene_at(&handoff, BOOT_ID, 9_999_999_999).unwrap_err(),
            Rejection::FutureSample
        );
        assert_eq!(
            scene_at(&handoff, BOOT_ID, 10_000_000_000 + MAX_HANDOFF_AGE_NS + 1).unwrap_err(),
            Rejection::Expired
        );
    }

    #[test]
    fn rejects_scene_time_overflow() {
        let record =
            valid_record().replace("scene-ns=42000000000", &format!("scene-ns={}", u64::MAX));
        let handoff = parse(&record).expect("well-formed handoff");
        assert_eq!(
            scene_at(&handoff, BOOT_ID, 10_000_000_001).unwrap_err(),
            Rejection::Overflow
        );
    }
}
