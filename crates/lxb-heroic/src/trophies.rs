//! An Epic game's achievements, and which of them this account has unlocked,
//! for the shell's Trophies column.
//!
//! legendary's own `achievements APP --json`, inside Heroic's sandbox and on
//! Heroic's session: the game's list out of Epic's store and the player's
//! unlocks beside it, one request each. What legendary answers is kept whole
//! except the dates, which are turned into seconds so the shell can write them
//! in the language it is set to, and the icons, which are fetched into the
//! shell's cache so the column draws them from the disk.
//!
//! **Which games.** Every one Epic defines achievements for, played or not,
//! here or not — the Steam half of the column lists what the account owns, and
//! this half does the same. legendary keeps the definitions beside each game's
//! record, so which games those are is read off the disk: about a quarter of a
//! library, sixty of 221 on the machine this was written on. A game it has not
//! written the count down for is asked about too, and the answer writes it.
//!
//! **What is kept goes first.** Every run of legendary is a second or two, so
//! the last answer for each game is said before any is asked for again: the
//! column has the whole list at once, and each game is brought up to date as
//! Epic answers. With no connection the kept answer is the answer.
//!
//! **Icons only when asked for by name.** A whole library's are thousands of
//! pictures, and nobody looks at most of them; the Steam half fetches a game's
//! icons when its list is opened, and so does this — `--only` names the game,
//! and is also how the shell asks again for a game that has just been played.
//! The pass over everything uses the icons already on the disk.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::Value;

use crate::heroic::{self, Paths};
use crate::library;
use crate::report::{Achievements, Installation, Reason, Trophy, PROTOCOL};

/// How many legendary runs at once. Each is a flatpak sandbox and a Python,
/// and four keep a library's minute short without the machine noticing.
const WORKERS: usize = 4;

/// Ask Epic for the achievements of the games worth asking about, one line
/// per game, then a line with `done` set. Whether anything was answered.
pub fn fetch(
    out: &mut impl Write,
    installation: &Installation,
    paths: &Paths,
    only: &[String],
) -> bool {
    let games: Vec<String> = library::read(paths)
        .into_iter()
        .filter(|game| match only.is_empty() {
            true => worth_asking(game),
            false => game.store.is_none() && only.contains(&game.app_name),
        })
        .map(|game| game.app_name)
        .collect();
    let root = crate::art::cache();
    let agent = crate::art::agent();
    // Named games are the ones somebody is looking at: their icons are
    // fetched. The pass over everything uses what is on the disk.
    let icons = !only.is_empty();
    if only.is_empty() {
        for kept in games
            .iter()
            .filter_map(|app_name| kept(root.as_deref()?, app_name))
        {
            if !say(out, &kept) {
                return false;
            }
        }
    }
    let next = AtomicUsize::new(0);
    let (tell, told) = std::sync::mpsc::channel::<Achievements>();
    let mut answered = 0usize;
    std::thread::scope(|scope| {
        for _ in 0..WORKERS {
            let tell = tell.clone();
            let (games, next, root, agent) = (&games, &next, &root, &agent);
            scope.spawn(move || loop {
                let at = next.fetch_add(1, Ordering::Relaxed);
                let Some(app_name) = games.get(at) else {
                    break;
                };
                let line = ask(installation, paths, app_name, root.as_deref(), agent, icons);
                if tell.send(line).is_err() {
                    break;
                }
            });
        }
        drop(tell);
        for line in told {
            answered += usize::from(line.reason.is_none());
            if !say(out, &line) {
                break;
            }
        }
    });
    say(
        out,
        &Achievements {
            protocol: PROTOCOL,
            app_name: String::new(),
            total: 0,
            unlocked: 0,
            xp: 0,
            total_xp: 0,
            list: Vec::new(),
            reason: None,
            done: true,
        },
    );
    answered > 0 || games.is_empty()
}

/// Whether a game is one the Trophies column asks Epic about: played
/// through Epic itself, and not a game Epic says has none. One whose count
/// legendary has not written down is asked, and the answer writes it.
fn worth_asking(game: &crate::report::Game) -> bool {
    game.store.is_none() && game.achievements != Some(0)
}

/// One game's achievements, with the icons fetched where `icons` says so and
/// taken from the disk where they are there already.
fn ask(
    installation: &Installation,
    paths: &Paths,
    app_name: &str,
    root: Option<&Path>,
    agent: &ureq::Agent,
    icons: bool,
) -> Achievements {
    let mut line = Achievements {
        protocol: PROTOCOL,
        app_name: app_name.to_string(),
        total: 0,
        unlocked: 0,
        xp: 0,
        total_xp: 0,
        list: Vec::new(),
        reason: None,
        done: false,
    };
    let out = heroic::output(
        heroic::legendary(installation, paths)
            .args(["achievements", app_name, "--json"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    );
    let out = match out {
        Ok(out) => out,
        Err(err) => {
            eprintln!("achievements: flatpak could not be run: {err}");
            line.reason = Some(Reason::NoHeroic);
            return line;
        }
    };
    let said = String::from_utf8_lossy(&out.stderr);
    // A game with none is answered with a sentence rather than JSON.
    if said.contains("No achievements found") {
        return line;
    }
    let Ok(answer) = serde_json::from_slice::<Value>(&out.stdout) else {
        for text in said.lines() {
            eprintln!("legendary: {text}");
        }
        line.reason = Some(library::why_legendary_failed(&said));
        // With no connection, what Epic said the last time it was asked: the
        // column goes on showing an offline machine's achievements, as it
        // does Steam's.
        if line.reason == Some(Reason::Offline) {
            if let Some(kept) = root.and_then(|root| kept(root, app_name)) {
                return kept;
            }
        }
        return line;
    };
    read(&answer, &mut line);
    let folder = root.and_then(|root| crate::art::folder(root, app_name));
    if let Some(folder) = folder.map(|folder| folder.join("achievements")) {
        for trophy in &mut line.list {
            trophy.icon = trophy
                .icon
                .take()
                .and_then(|url| icon(agent, &folder, &url, icons));
        }
    }
    if let Some(root) = root {
        keep(root, &line);
    }
    line
}

/// The file a game's last answer is kept in, beside its icons.
const KEPT: &str = "achievements.json";

/// Keep what Epic answered about a game, for when it cannot be asked.
/// Written beside and then moved over, so a reader never finds half of one.
fn keep(root: &Path, line: &Achievements) {
    let Some(folder) = crate::art::folder(root, &line.app_name) else {
        return;
    };
    let Ok(json) = serde_json::to_vec(line) else {
        return;
    };
    let partial = folder.join(format!(".{KEPT}.part"));
    let kept = std::fs::create_dir_all(&folder)
        .and_then(|()| std::fs::write(&partial, json))
        .and_then(|()| std::fs::rename(&partial, folder.join(KEPT)));
    if let Err(why) = kept {
        eprintln!("achievements: not kept for {}: {why}", line.app_name);
    }
}

/// What was kept about a game, where there is anything and it is this game's.
fn kept(root: &Path, app_name: &str) -> Option<Achievements> {
    let at = crate::art::folder(root, app_name)?.join(KEPT);
    let mut line: Achievements = serde_json::from_slice(&std::fs::read(at).ok()?).ok()?;
    if line.app_name != app_name || line.reason.is_some() || line.done {
        return None;
    }
    line.protocol = PROTOCOL;
    Some(line)
}

/// legendary's answer, into the record the shell reads.
fn read(answer: &Value, line: &mut Achievements) {
    let number = |value: &Value| value.as_u64().unwrap_or(0).min(u64::from(u32::MAX)) as u32;
    line.total = number(&answer["total_achievements"]);
    line.total_xp = number(&answer["total_product_xp"]);
    line.unlocked = number(&answer["user_unlocked"]);
    line.xp = number(&answer["user_xp"]);
    for (group, hidden) in [
        ("completed", false),
        ("in_progress", false),
        ("uninitiated", false),
        ("hidden", true),
    ] {
        let Some(list) = answer[group].as_array() else {
            continue;
        };
        for item in list {
            let text = |key: &str| item[key].as_str().unwrap_or_default().to_string();
            line.list.push(Trophy {
                name: text("name"),
                title: text("display_name"),
                description: text("description"),
                unlocked: item["unlocked"].as_bool() == Some(true),
                unlocked_at: item["unlock_date"].as_str().and_then(seconds_of),
                icon: item["icon_link"].as_str().map(str::to_string),
                xp: number(&item["xp"]),
                rarity: item["rarity"]["percent"]
                    .as_f64()
                    .map(|percent| percent as f32),
                hidden: hidden || item["hidden"].as_bool() == Some(true),
            });
        }
    }
}

/// One icon on this disk, fetched if it is not there yet and `fetch` says to:
/// Epic's own name for it, the last part of its address, where that is a
/// plain one.
fn icon(agent: &ureq::Agent, folder: &Path, url: &str, fetch: bool) -> Option<String> {
    let name = url.rsplit('/').next()?;
    let plain = !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
        && !name.starts_with('.');
    if !plain {
        return None;
    }
    let at: PathBuf = folder.join(name);
    if !at.is_file() {
        if !fetch {
            return None;
        }
        if let Err(why) = crate::art::fetch_to(agent, url, &at) {
            eprintln!("achievements: an icon: {why}");
            return None;
        }
    }
    Some(at.to_string_lossy().into_owned())
}

/// An ISO 8601 date in UTC — `2023-04-05T18:30:12.345Z` — as seconds since
/// the epoch.
fn seconds_of(date: &str) -> Option<u64> {
    let (day, time) = date.split_once('T')?;
    let mut day = day.splitn(3, '-').map(|part| part.parse::<i64>().ok());
    let (year, month, date) = (day.next()??, day.next()??, day.next()??);
    let time = time.trim_end_matches('Z');
    let time = time.split(['+', '.']).next()?;
    let mut clock = time.splitn(3, ':').map(|part| part.parse::<i64>().ok());
    let (hour, minute, second) = (clock.next()??, clock.next()??, clock.next()??);
    if !(1..=12).contains(&month) || !(1..=31).contains(&date) {
        return None;
    }
    // Howard Hinnant's days-from-civil.
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + date - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let seconds = days * 86_400 + hour * 3_600 + minute * 60 + second;
    u64::try_from(seconds).ok()
}

fn say(out: &mut impl Write, line: &Achievements) -> bool {
    let Ok(json) = serde_json::to_string(line) else {
        return false;
    };
    writeln!(out, "{json}").and_then(|()| out.flush()).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Every game Epic defines achievements for is asked about, played or
    /// not, here or not; a game with none is not, nor one another store
    /// plays, and one whose count is not written down yet is.
    #[test]
    fn every_game_with_achievements_is_worth_asking_about() {
        let game = |achievements, store| crate::report::Game {
            app_name: "Quail".into(),
            title: "Quail".into(),
            developer: None,
            cover: None,
            hero: None,
            logo: None,
            cover_file: None,
            hero_file: None,
            logo_file: None,
            installed: None,
            store,
            offline: false,
            cloud_saves: false,
            played_minutes: 0,
            last_played: None,
            achievements,
        };
        assert!(
            worth_asking(&game(Some(12), None)),
            "never played, not here"
        );
        assert!(worth_asking(&game(None, None)), "not counted yet");
        assert!(!worth_asking(&game(Some(0), None)));
        assert!(!worth_asking(&game(
            Some(12),
            Some(crate::report::Store::Ea)
        )));
    }

    /// What Epic last answered is kept, and handed back as it was — and a
    /// file that is not one of these, or is another game's, is not.
    #[test]
    fn the_last_answer_is_kept_for_when_epic_cannot_be_asked() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let line = Achievements {
            protocol: PROTOCOL,
            app_name: "Quail".to_string(),
            total: 12,
            unlocked: 3,
            xp: 150,
            total_xp: 1000,
            list: vec![Trophy {
                name: "a".into(),
                title: "First".into(),
                description: String::new(),
                unlocked: true,
                unlocked_at: Some(1_790_295_142),
                icon: Some("/cache/Quail/achievements/025b0937".into()),
                xp: 50,
                rarity: Some(12.5),
                hidden: false,
            }],
            reason: None,
            done: false,
        };
        assert!(kept(root, "Quail").is_none(), "nothing kept yet");
        keep(root, &line);
        let back = kept(root, "Quail").expect("the kept answer");
        assert_eq!((back.total, back.unlocked, back.xp), (12, 3, 150));
        assert_eq!(back.list[0].icon, line.list[0].icon);
        assert!(!root.join("Quail").join(".achievements.json.part").exists());

        std::fs::create_dir_all(root.join("Other")).unwrap();
        std::fs::copy(root.join("Quail").join(KEPT), root.join("Other").join(KEPT)).unwrap();
        assert!(kept(root, "Other").is_none(), "another game's answer");
        std::fs::write(root.join("Quail").join(KEPT), "{").unwrap();
        assert!(kept(root, "Quail").is_none(), "half a file");
        assert!(kept(root, "../Quail").is_none());
    }

    #[test]
    fn epics_dates_are_seconds_since_the_epoch() {
        assert_eq!(seconds_of("1970-01-01T00:00:00.000Z"), Some(0));
        assert_eq!(seconds_of("2026-09-25T00:12:22.152Z"), Some(1_790_295_142));
        assert_eq!(seconds_of("2000-02-29T12:00:00Z"), Some(951_825_600));
        assert_eq!(seconds_of("yesterday"), None);
        assert_eq!(seconds_of("2026-13-01T00:00:00Z"), None);
    }

    /// legendary's own shape, as it answered for Cat Quest on 2026-09-25.
    #[test]
    fn legendarys_answer_is_read_group_by_group() {
        let answer = json!({
            "total_achievements": 3, "total_product_xp": 1000,
            "user_unlocked": 1, "user_xp": 50,
            "completed": [{"name": "a", "display_name": "First", "description": "d",
                           "unlocked": true, "unlock_date": "2026-09-25T00:12:22.152Z",
                           "icon_link": "https://x/icons/025b0937", "xp": 50,
                           "rarity": {"percent": 12.5}, "hidden": false}],
            "in_progress": [],
            "uninitiated": [{"name": "b", "display_name": "Second", "unlocked": false,
                             "unlock_date": null, "xp": 100, "hidden": false}],
            "hidden": [{"name": "c", "display_name": "Secret", "unlocked": false, "xp": 850}]
        });
        let mut line = Achievements {
            protocol: PROTOCOL,
            app_name: "Quail".into(),
            total: 0,
            unlocked: 0,
            xp: 0,
            total_xp: 0,
            list: Vec::new(),
            reason: None,
            done: false,
        };
        read(&answer, &mut line);
        assert_eq!(
            (line.total, line.unlocked, line.xp, line.total_xp),
            (3, 1, 50, 1000)
        );
        assert_eq!(line.list.len(), 3);
        assert!(line.list[0].unlocked);
        assert_eq!(line.list[0].unlocked_at, Some(1_790_295_142));
        assert_eq!(line.list[0].rarity, Some(12.5));
        assert!(!line.list[1].unlocked && !line.list[1].hidden);
        assert!(line.list[2].hidden);
    }

    #[test]
    fn only_a_plain_icon_name_is_a_file() {
        let agent = crate::art::agent();
        let dir = tempfile::tempdir().unwrap();
        let already = dir.path().join("025b0937");
        std::fs::write(&already, b"\x89PNG\r\n\x1a\n").unwrap();
        for fetch in [true, false] {
            assert_eq!(
                icon(&agent, dir.path(), "https://x/icons/025b0937", fetch).as_deref(),
                Some(already.to_str().unwrap()),
                "one on the disk is used whether or not fetching"
            );
        }
        assert_eq!(icon(&agent, dir.path(), "https://x/icons/..", true), None);
        assert_eq!(icon(&agent, dir.path(), "https://x/icons/a b", true), None);
        // Not on the disk and not to be fetched: nothing, and nothing asked.
        assert_eq!(
            icon(&agent, dir.path(), "https://x/icons/elsewhere", false),
            None
        );
        assert!(!dir.path().join("elsewhere").exists());
    }
}
