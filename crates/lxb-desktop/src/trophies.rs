//! The Trophies column. Provider identities stay separate from launchable games.
use crate::apps::{About, Entry, Facts};
use lxb_steam::achievements::{self, Heard, Progress, ProgressHeard, Snapshot};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::time::{Duration, Instant};

pub const COLUMN: &str = "trophies";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Key {
    RetroConfigure,
    RetroGame(u32, String),
    RetroAchievement(u32, u32),
    RetroStatus(u32, String),
    SteamGame(u32),
    SteamAchievement(u32, String),
    Status(u32, &'static str),
}

/// The machine a game is for, as the column groups by it.
///
/// A name and the mark that goes with it, together because they are one answer
/// and a folder needs both: a column of consoles with no marks is a list, and a
/// column of marks with no names is a puzzle. The mark is a name the shell's
/// atlas knows — `lxb:console-nes` — supplied by whichever half of the column
/// the row came from rather than looked up here, because the table of consoles
/// belongs to the RetroArch package and this shell deliberately keeps no copy
/// of it. See [`Sort::Platform`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Platform {
    pub name: String,
    pub mark: String,
}

/// What Valve's half of the column is called where it is a machine among
/// machines. Its own name and not "Valve" or "PC": it is the name on the
/// category the games come from, and the one the person chose it by.
pub const STEAM_PLATFORM: &str = "Steam";

impl Platform {
    /// Valve's half, as a machine.
    pub fn steam() -> Self {
        Self {
            name: STEAM_PLATFORM.to_string(),
            mark: crate::icons::CATEGORY_STEAM.to_string(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Row {
    pub key: Key,
    pub facts: Facts,
    pub picture: Option<PathBuf>,
    pub entries: Option<Vec<Entry>>,
    /// Section label attached to its achievements, never a cursor destination.
    pub section: Option<String>,
    /// The shape this row's picture is — the width of it over its height —
    /// where that is not the shape of the column.
    ///
    /// `None` for every Steam row, because Valve publishes one cover shape and
    /// the column is already cut to it. A RetroAchievements row carries one
    /// because its picture is a photograph of a box, and the boxes are not the
    /// same shape: a PlayStation jewel case is very nearly square, a UMD case
    /// is half again as tall as it is wide, and one card shape for the two of
    /// them is a card neither picture can fill. Measured off the pictures
    /// themselves rather than tabled — see [`crate::retroarch::shelf_shape`],
    /// which is where a ROM shelf gets the same answer.
    ///
    /// It decides the card's shape and nothing else. The room a row is given
    /// belongs to the column, so a shape that cannot fill that room is drawn
    /// smaller inside it and no row ever moves: see `ui::card_of`.
    pub shape: Option<f32>,
    /// Whether there is a copy of this game on this disk to be played, where
    /// this row is the one that knows.
    ///
    /// `None` for a Steam row, whose own library answers — that answer changes
    /// while the shell is running, and a download finishing is a game moving
    /// to the top of [`Sort::InstalledFirst`]. A RetroAchievements row knows
    /// when it is built and never changes its mind: a ROM found in the chosen
    /// folder is here, and a game the account has unlocked something in
    /// somewhere else is not — there is nothing on this machine to start.
    ///
    /// `None` too for every row that is not a game and is never sorted: an
    /// achievement, a status line, the Configure invitation.
    pub installed: Option<bool>,
    /// The machine this game is for, where this row is a game.
    ///
    /// Set by whichever half of the column built the row — Steam for a Steam
    /// row, the console for a RetroAchievements one — and `None` for every row
    /// that is not a game. It is what [`Sort::Platform`] groups by, and a game
    /// whose console this machine cannot name carries `None` as well: those are
    /// gathered under one folder that says so, rather than given a name nobody
    /// wrote down.
    pub platform: Option<Platform>,
}
impl Row {
    pub fn game(&self) -> Option<u32> {
        match self.key {
            Key::SteamGame(id) => Some(id),
            _ => None,
        }
    }
}

/// Match the visible neighborhood of all columns, including ones animating away.
pub fn drawn_rows<'a>(column: &crate::model::Column<'a>) -> impl Iterator<Item = &'a Entry> {
    let center = column.position.max(0.0) as usize;
    let from = center.saturating_sub(8).min(column.entries.len());
    let to = center.saturating_add(9).min(column.entries.len());
    let entries = column.entries;
    (from..to)
        .chain(std::iter::once(column.selected))
        .filter_map(move |i| entries.get(i))
}

pub fn section(entries: &[Entry], selected: usize) -> Option<&str> {
    match entries.get(selected)? {
        Entry::Trophy(row) => row.section.as_deref(),
        _ => None,
    }
}

/// Stable identities for the complete browsing path, including the letter index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Position {
    Trophy(Key),
    Index(String),
    Platform(String),
    Search(crate::apps::Role),
}
impl Position {
    pub fn of(entry: &Entry) -> Option<Self> {
        match entry {
            Entry::Trophy(row) => Some(Self::Trophy(row.key.clone())),
            // The two kinds of folder this column has are told apart by which
            // half of them holds the thing that does not move. A platform's
            // folder is *named* after the platform and says how many games are
            // on it underneath, so the name is the identity. A letter's is
            // named by the count — "12 games", with the letter as its mark — so
            // its name changes the moment somebody types in the search field,
            // and the mark is what stays put.
            Entry::Folder(folder) if folder.comment.is_some() => {
                Some(Self::Platform(folder.title.clone()))
            }
            Entry::Folder(folder) => Some(Self::Index(
                folder.icon.clone().unwrap_or_else(|| folder.title.clone()),
            )),
            Entry::Search(search) => Some(Self::Search(search.role)),
            _ => None,
        }
    }
}

struct Page {
    request: u64,
    asked: Instant,
    loading: bool,
    snapshot: Option<Snapshot>,
    error: Option<String>,
}

/// What one account's cache holds: counts for every game, pages for the opened ones.
type Restored = (BTreeMap<u32, Progress>, BTreeMap<u32, Snapshot>);

#[derive(Default)]
pub struct Trophies {
    account: Option<u64>,
    serial: u64,
    pages: BTreeMap<u32, Page>,
    watching: BTreeSet<u32>,
    pictures: Pictures,
    progress: BTreeMap<u32, Progress>,
    restoring: Option<Receiver<Restored>>,
    library: BTreeSet<u32>,
    pending: VecDeque<u32>,
    progress_request: Option<(u64, Instant)>,
    progress_after: Option<Instant>,
}
impl Trophies {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn account(&mut self, account: Option<u64>) {
        if self.account != account {
            self.account = account;
            self.pages.clear();
            self.watching.clear();
            self.progress.clear();
            self.library.clear();
            self.pending.clear();
            self.progress_request = None;
            self.progress_after = None;
            self.restoring = account.map(|account| {
                let (send, receive) = mpsc::channel();
                std::thread::spawn(move || {
                    let _ = send.send(achievements::saved(account));
                });
                receive
            });
        }
    }

    pub fn library(&mut self, games: &[lxb_steam::Game]) {
        let ids: BTreeSet<_> = games.iter().map(|g| g.app_id).collect();
        self.pending.extend(ids.difference(&self.library).copied());
        self.pending.retain(|id| ids.contains(id));
        self.library = ids;
    }

    fn restore(&mut self) -> bool {
        let Some(receive) = &self.restoring else {
            return false;
        };
        let saved = match receive.try_recv() {
            Ok(saved) => saved,
            Err(mpsc::TryRecvError::Empty) => return false,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.restoring = None;
                return false;
            }
        };
        self.restoring = None;
        for (app, progress) in saved.0 {
            self.progress.entry(app).or_insert(progress);
        }
        for (app, snapshot) in saved.1 {
            let page = self.pages.entry(app).or_insert(Page {
                request: 0,
                asked: Instant::now(),
                loading: false,
                snapshot: None,
                error: None,
            });
            if page.snapshot.is_none() {
                page.snapshot = Some(snapshot);
            }
        }
        true
    }

    pub fn progress_heard(&mut self, heard: ProgressHeard) -> bool {
        if self.account != Some(heard.account)
            || self.progress_request.map(|p| p.0) != Some(heard.request)
        {
            return false;
        }
        self.progress_request = None;
        if let Ok(values) = heard.result {
            for (app, progress) in values {
                // A per-game request may have finished after this bulk request began.
                if self
                    .progress
                    .get(&app)
                    .is_none_or(|old| old.fetched_at <= progress.fetched_at)
                {
                    self.progress.insert(app, progress);
                }
            }
            true
        } else {
            // Back off the whole queue if Steam refuses the service.
            self.pending.clear();
            self.progress_after = Some(Instant::now() + Duration::from_secs(300));
            false
        }
    }

    fn next_progress(&mut self, online: bool) -> Option<(Vec<u32>, u64)> {
        if !online || self.restoring.is_some() {
            return None;
        }
        let now = Instant::now();
        if let Some((_, asked)) = self.progress_request {
            if now.duration_since(asked) < Duration::from_secs(45) {
                return None;
            }
            self.progress_request = None;
        }
        if self.pending.is_empty() && self.progress_after.is_some_and(|at| now >= at) {
            self.pending.extend(self.library.iter().copied());
        }
        if self.pending.is_empty() {
            return None;
        }
        let batch: Vec<_> = self.pending.drain(..self.pending.len().min(100)).collect();
        self.serial += 1;
        self.progress_request = Some((self.serial, now));
        self.progress_after = Some(now + Duration::from_secs(300));
        Some((batch, self.serial))
    }

    /// Called with games actually open on any display, never the whole library.
    pub fn watch(&mut self, games: BTreeSet<u32>, client: &lxb_steam::Steam, online: bool) -> bool {
        let now = Instant::now();
        let mut changed = self.pictures.take() | self.restore();
        if self.account.is_none() {
            return changed;
        }
        if let Some((batch, request)) = self.next_progress(online) {
            client.achievement_progress(batch, request);
        }
        for &app_id in &games {
            if self.progress.get(&app_id).is_some_and(|p| p.total == 0) {
                continue;
            }
            let entering = !self.watching.contains(&app_id);
            let page = self.pages.entry(app_id).or_insert_with(|| Page {
                request: 0,
                asked: now,
                loading: false,
                snapshot: None,
                error: None,
            });
            if page.loading && now.duration_since(page.asked) > Duration::from_secs(45) {
                page.loading = false;
                page.error = Some(
                    crate::i18n::text(
                        "shell-steam-took-too-long-go-back-and-reopen-this-game-to-retry",
                    )
                    .into(),
                );
                changed = true;
            }
            let refresh = page.request == 0
                || entering
                || now.duration_since(page.asked) > Duration::from_secs(300);
            if !page.loading && refresh {
                self.serial += 1;
                page.request = self.serial;
                page.asked = now;
                page.loading = true;
                page.error = None;
                client.achievements(app_id, page.request);
                changed = true;
            }
        }
        self.watching = games;
        changed
    }

    pub fn reconnected(&mut self) {
        self.watching.clear();
        self.progress_request = None;
        self.pending = self.library.iter().copied().collect();
        self.progress_after = None;
        for page in self.pages.values_mut() {
            page.loading = false;
        }
    }

    pub fn heard(&mut self, heard: Heard) -> bool {
        if self.account != Some(heard.account) {
            return false;
        }
        let Some(page) = self.pages.get_mut(&heard.app_id) else {
            return false;
        };
        if page.request != heard.request || !page.loading {
            return false;
        }
        page.loading = false;
        match heard.result {
            Ok(snapshot) => {
                let progress = Progress {
                    unlocked: snapshot.achievements.iter().filter(|a| a.achieved).count() as u32,
                    total: snapshot.achievements.len() as u32,
                    fetched_at: snapshot.fetched_at,
                };
                if self
                    .progress
                    .get(&heard.app_id)
                    .is_none_or(|old| old.fetched_at <= progress.fetched_at)
                {
                    self.progress.insert(heard.app_id, progress);
                }
                page.error = snapshot.stale.clone();
                page.snapshot = Some(snapshot);
            }
            Err(reason) => page.error = Some(reason),
        }
        true
    }

    pub fn want_icons(&mut self, keys: &[(u32, String)]) {
        for (app, icon) in keys {
            self.pictures.want(*app, icon);
        }
    }

    pub fn icon_for(&self, key: &Key) -> Option<(u32, String)> {
        let Key::SteamAchievement(app, id) = key else {
            return None;
        };
        let snapshot = self.pages.get(app)?.snapshot.as_ref()?;
        let achievement = snapshot.achievements.iter().find(|a| &a.apiname == id)?;
        if achievement.hidden && !achievement.achieved {
            return None;
        }
        let icon = if achievement.achieved {
            &achievement.icon
        } else {
            &achievement.icon_gray
        };
        Some((*app, icon.as_ref()?.clone()))
    }

    pub fn rows(&self, games: &[lxb_steam::Game], sort: lxb_steam::library::Sort) -> Vec<Entry> {
        if self.account.is_none() {
            return Vec::new();
        }
        lxb_steam::library::sorted_by(games.to_vec(), sort)
            .iter()
            .map(|game| {
                let app = game.app_id;
                let page = self.pages.get(&app);
                let snapshot = page.and_then(|p| p.snapshot.as_ref());
                let comment = match self.progress.get(&app) {
                    Some(p) if p.total == 0 => crate::i18n::text("shell-steam-no-achievements").into(),
                    Some(p) => crate::message!("steam-achievements-unlocked", "unlocked" => p.unlocked, "total" => p.total),
                    None => match snapshot {
                        Some(s) => crate::message!("steam-achievements-unlocked", "unlocked" => s.achievements.iter().filter(|a| a.achieved).count(), "total" => s.achievements.len()),
                        None => crate::i18n::text("shell-steam-achievement-counts-pending").into(),
                    },
                };
                let mut entries = Vec::new();
                if snapshot.is_none() {
                    if self.progress.get(&app).is_some_and(|p| p.total == 0) {
                        entries.push(status(
                            app,
                            "empty",
                            crate::i18n::text("shell-no-achievements"),
                            crate::i18n::text("shell-steam-lists-no-achievements-for-this-game"),
                        ));
                    } else if let Some(error) = page.and_then(|p| p.error.as_ref()) {
                        entries.push(status(
                            app,
                            "error",
                            crate::i18n::text("shell-achievements-unavailable"),
                            &crate::message!("trophies-error-retry", "error" => error),
                        ));
                    }
                }
                if let Some(snapshot) = snapshot {
                    if snapshot.achievements.is_empty() {
                        entries.push(status(
                            app,
                            "empty",
                            crate::i18n::text("shell-no-achievements"),
                            crate::i18n::text("shell-steam-lists-no-achievements-for-this-game"),
                        ));
                    } else {
                        for (unlocked, hidden, label) in [
                            (true, false, crate::i18n::text("shell-unlocked")),
                            (false, false, crate::i18n::text("shell-locked")),
                            (false, true, "Hidden"),
                        ] {
                            let group: Vec<_> = snapshot
                                .achievements
                                .iter()
                                .filter(|a| {
                                    a.achieved == unlocked && (unlocked || a.hidden == hidden)
                                })
                                .collect();
                            if group.is_empty() {
                                continue;
                            }
                            let section = format!("{label} ({})", group.len());
                            for achievement in group {
                                let concealed = achievement.hidden && !achievement.achieved;
                                let title =
                                    achievement.name.as_deref().unwrap_or(&achievement.apiname);
                                let description = achievement.description.as_deref().unwrap_or("");
                                let state = if achievement.achieved {
                                    unlocked_at(achievement.unlocktime)
                                        .unwrap_or_else(|| crate::i18n::text("shell-unlocked").into())
                                } else {
                                    crate::i18n::text("shell-locked").into()
                                };
                                let key = Key::SteamAchievement(app, achievement.apiname.clone());
                                let picture = self
                                    .icon_for(&key)
                                    .and_then(|key| self.pictures.ready.get(&key).cloned());
                                let mut details = vec![
                                    (crate::i18n::text("shell-achievement").into(), title.into()),
                                    (crate::i18n::text("shell-description").into(), description.into()),
                                    (crate::i18n::text("shell-status").into(), state.clone()),
                                ];
                                if let Some(percent) =
                                    snapshot.percentages.get(&achievement.apiname)
                                {
                                    details.push((
                                        crate::i18n::text("shell-players-unlocked").into(),
                                        format!("{percent:.1}%"),
                                    ));
                                }
                                entries.push(Entry::Trophy(Row {
                                    key,
                                    section: Some(section.clone()),
                                    shape: None,
                                    installed: None,
                                    platform: None,
                                    picture,
                                    entries: None,
                                    facts: Facts {
                                        title: if concealed {
                                            crate::i18n::text("shell-hidden-achievement").into()
                                        } else {
                                            title.into()
                                        },
                                        comment: if concealed {
                                            crate::i18n::text("shell-press-to-reveal-details").into()
                                        } else if description.is_empty() {
                                            state
                                        } else {
                                            format!("{description} · {state}")
                                        },
                                        icon: crate::icons::CATEGORY_TROPHIES.into(),
                                        about: About::Listed(details),
                                    },
                                }));
                            }
                        }
                    }
                }
                if entries.is_empty() {
                    entries.push(status(
                        app,
                        "loading",
                        crate::i18n::text("shell-loading-achievements"),
                        crate::i18n::text("shell-asking-steam-for-this-game-s-achievements"),
                    ));
                }
                Entry::Trophy(Row {
                    section: None,
                    shape: None,
                    installed: None,
                    platform: Some(Platform::steam()),
                    key: Key::SteamGame(app),
                    picture: None,
                    entries: Some(entries),
                    facts: Facts {
                        title: game.name.clone(),
                        comment,
                        icon: crate::icons::STEAM.into(),
                        about: About::Listed(Vec::new()),
                    },
                })
            })
            .collect()
    }
}

fn status(app: u32, key: &'static str, title: &str, comment: &str) -> Entry {
    Entry::Trophy(Row {
        section: None,
        shape: None,
        installed: None,
        platform: None,
        key: Key::Status(app, key),
        picture: None,
        entries: None,
        facts: Facts {
            title: title.into(),
            comment: comment.into(),
            icon: crate::icons::CATEGORY_TROPHIES.into(),
            about: About::Listed(Vec::new()),
        },
    })
}

fn unlocked_at(seconds: u64) -> Option<String> {
    if seconds == 0 {
        return None;
    }
    let seconds: libc::time_t = seconds.try_into().ok()?;
    let mut tm = std::mem::MaybeUninit::<libc::tm>::uninit();
    // SAFETY: both pointers are valid; read tm only when localtime_r succeeded.
    if unsafe { libc::localtime_r(&seconds, tm.as_mut_ptr()) }.is_null() {
        return None;
    }
    let tm = unsafe { tm.assume_init() };
    Some(
        crate::message!("achievement-unlocked-at", "year" => format!("{:04}", tm.tm_year + 1900), "month" => format!("{:02}", tm.tm_mon + 1), "day" => format!("{:02}", tm.tm_mday), "hour" => format!("{:02}", tm.tm_hour), "minute" => format!("{:02}", tm.tm_min)),
    )
}

type ImageKey = (u32, String);
/// The download thread's two ends: names to fetch, and what came back.
type IconWorker = (SyncSender<ImageKey>, Receiver<(ImageKey, Option<PathBuf>)>);
#[derive(Default)]
struct Pictures {
    worker: Option<IconWorker>,
    pending: BTreeSet<ImageKey>,
    tried: BTreeMap<ImageKey, Instant>,
    ready: BTreeMap<ImageKey, PathBuf>,
}
impl Pictures {
    fn want(&mut self, app: u32, name: &str) {
        let key = (app, name.to_owned());
        if self.ready.contains_key(&key)
            || self.pending.contains(&key)
            || self
                .tried
                .get(&key)
                .is_some_and(|t| t.elapsed() < Duration::from_secs(60))
            || achievements::icon_url(app, name).is_none()
        {
            return;
        }
        if self.worker.is_none() {
            let (send, receive) = mpsc::sync_channel::<ImageKey>(24);
            let (done, take) = mpsc::channel();
            std::thread::spawn(move || {
                let cdn = lxb_steam::art::Cdn::new();
                while let Ok(key) = receive.recv() {
                    let path = (|| {
                        let url = achievements::icon_url(key.0, &key.1)?;
                        let dir = achievements::cache_root()?
                            .join("icons")
                            .join(key.0.to_string());
                        let path = dir.join(&key.1);
                        if !std::fs::read(&path).is_ok_and(|b| valid_icon(&b)) {
                            let bytes = cdn.get(&url).ok()?;
                            if !valid_icon(&bytes) {
                                return None;
                            }
                            std::fs::create_dir_all(&dir).ok()?;
                            let temp = path.with_extension("download");
                            std::fs::write(&temp, bytes).ok()?;
                            std::fs::rename(temp, &path).ok()?;
                        }
                        Some(path)
                    })();
                    if done.send((key, path)).is_err() {
                        break;
                    }
                }
            });
            self.worker = Some((send, take));
        }
        if self
            .worker
            .as_ref()
            .unwrap()
            .0
            .try_send(key.clone())
            .is_ok()
        {
            self.pending.insert(key);
        }
    }
    fn take(&mut self) -> bool {
        let mut changed = false;
        if let Some((_, receive)) = &self.worker {
            for (key, path) in receive.try_iter() {
                self.pending.remove(&key);
                self.tried.insert(key.clone(), Instant::now());
                if let Some(path) = path {
                    self.ready.insert(key, path);
                    changed = true;
                }
            }
        }
        changed
    }
}

fn valid_icon(bytes: &[u8]) -> bool {
    if bytes.is_empty() || bytes.len() > 2 * 1024 * 1024 {
        return false;
    }
    let dimensions = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()
        .and_then(|r| r.into_dimensions().ok());
    dimensions.is_some_and(|(w, h)| w > 0 && h > 0 && w <= 1024 && h <= 1024)
        && image::load_from_memory(bytes).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn snapshot() -> Snapshot {
        Snapshot {
            achievements: vec![achievements::Achievement {
                apiname: "secret".into(),
                name: Some("Spoiler title".into()),
                description: Some("Spoiler description".into()),
                achieved: false,
                unlocktime: 0,
                icon: Some("color.jpg".into()),
                icon_gray: Some("gray.jpg".into()),
                hidden: true,
                schema_order: 0,
            }],
            percentages: BTreeMap::new(),
            fetched_at: 1,
            stale: None,
        }
    }
    fn page() -> Page {
        Page {
            request: 7,
            asked: Instant::now(),
            loading: true,
            snapshot: None,
            error: None,
        }
    }
    #[test]
    fn trophy_library_has_search_and_a_filtered_alphabetical_index() {
        let store = Trophies {
            account: Some(1),
            ..Default::default()
        };
        let games = vec![
            lxb_steam::Game::invented(1, "Portal".into(), false),
            lxb_steam::Game::invented(2, "Portal 2".into(), true),
            lxb_steam::Game::invented(3, "112 Operator".into(), false),
        ];
        let mut browser = Browser {
            sort: Sort::NameAscending,
            search: String::new(),
        };
        let rows = browser.rows(
            store.rows(&games, lxb_steam::library::Sort::InstalledFirst),
            None,
            |id| {
                games
                    .iter()
                    .find(|g| g.app_id == id)
                    .map(|game| game.installed)
            },
        );
        assert_eq!(crate::apps::head_rows(&rows), 2);
        assert_eq!(rows[0].title(), "Search");
        assert_eq!(rows[1].title(), "Alphabetical");
        assert!(matches!(&rows[0], Entry::Search(s) if s.of == crate::apps::Searched::Trophies));
        assert_eq!(
            rows[2..].iter().map(Entry::title).collect::<Vec<_>>(),
            ["112 Operator", "Portal", "Portal 2"]
        );
        let letters = rows[1].entries().unwrap();
        assert_eq!(
            letters.iter().map(Entry::icon).collect::<Vec<_>>(),
            [
                crate::icons::letter_mark('#'),
                crate::icons::letter_mark('P')
            ]
        );
        assert_eq!(
            letters[1]
                .entries()
                .unwrap()
                .iter()
                .map(Entry::title)
                .collect::<Vec<_>>(),
            ["Portal 2", "Portal"]
        );
        assert!(letters[1]
            .entries()
            .unwrap()
            .iter()
            .all(|e| matches!(e, Entry::Trophy(_)) && !e.starts_something()));
        let mut cursor = crate::model::Cursor::for_model(&crate::model::Lattice::new(vec![
            crate::apps::Category {
                id: COLUMN,
                title: "Trophies",
                icon: crate::icons::CATEGORY_TROPHIES,
                entries: rows.clone(),
            },
        ]));
        assert_eq!(
            cursor.selected_item(),
            2,
            "navigation starts at the first game"
        );
        browser.search = "PORTAL 2".into();
        let matched = browser.rows(
            store.rows(&games, lxb_steam::library::Sort::InstalledFirst),
            None,
            |id| {
                games
                    .iter()
                    .find(|g| g.app_id == id)
                    .map(|game| game.installed)
            },
        );
        assert_eq!(
            matched
                .iter()
                .filter(|e| matches!(e, Entry::Trophy(_)))
                .count(),
            1
        );
        assert!(matched
            .iter()
            .any(|e| matches!(e, Entry::Search(s) if s.role == crate::apps::Role::Clear)));
        let index = matched
            .iter()
            .find(|e| e.title() == "Alphabetical")
            .unwrap();
        assert_eq!(index.entries().unwrap().len(), 1);
        assert_eq!(
            index.entries().unwrap()[0].entries().unwrap()[0].title(),
            "Portal 2"
        );
        browser.search = "not in this library".into();
        let empty = browser.rows(
            store.rows(&games, lxb_steam::library::Sort::InstalledFirst),
            None,
            |id| {
                games
                    .iter()
                    .find(|g| g.app_id == id)
                    .map(|game| game.installed)
            },
        );
        assert!(empty.iter().all(|e| matches!(e, Entry::Search(_))));
        browser.search = "".into();
        assert_eq!(
            browser
                .rows(
                    store.rows(&games, lxb_steam::library::Sort::InstalledFirst),
                    None,
                    |id| games
                        .iter()
                        .find(|g| g.app_id == id)
                        .map(|game| game.installed)
                )
                .len(),
            rows.len()
        );
        // Also confirm the initially selected result is reachable through the cursor.
        let lattice = crate::model::Lattice::new(vec![crate::apps::Category {
            id: COLUMN,
            title: "Trophies",
            icon: crate::icons::CATEGORY_TROPHIES,
            entries: rows,
        }]);
        assert!(cursor.enter(&lattice));
    }

    #[test]
    fn refresh_preserves_the_full_alphabetical_path_and_filtered_paths_close() {
        use crate::model::{Cursor, Lattice};
        let mut browser = Browser::default();
        let mut store = Trophies {
            account: Some(1),
            ..Default::default()
        };
        let mut snapshot = snapshot();
        let mut extra = snapshot.achievements[0].clone();
        extra.apiname = "second".into();
        snapshot.achievements.push(extra);
        store.pages.insert(
            2,
            Page {
                snapshot: Some(snapshot),
                ..page()
            },
        );
        let mut games = vec![
            lxb_steam::Game::invented(1, "Portal".into(), false),
            lxb_steam::Game::invented(2, "Portal 2".into(), false),
        ];
        let mut lattice = Lattice::new(vec![crate::apps::Category {
            id: COLUMN,
            title: "Trophies",
            icon: crate::icons::CATEGORY_TROPHIES,
            entries: browser.rows(
                store.rows(&games, lxb_steam::library::Sort::InstalledFirst),
                None,
                |id| {
                    games
                        .iter()
                        .find(|g| g.app_id == id)
                        .map(|game| game.installed)
                },
            ),
        }]);
        let mut cursor = Cursor::for_model(&lattice);
        cursor.point_at_row(1, &lattice); // Alphabetical
        assert!(cursor.enter(&lattice));
        assert!(cursor.enter(&lattice)); // P
        cursor.point_at_row(1, &lattice);
        assert!(cursor.enter(&lattice)); // Portal 2
        cursor.point_at_row(1, &lattice);
        let selected = cursor.trophy_selection(&lattice).unwrap();
        games.push(lxb_steam::Game::invented(3, "Alpha".into(), false));
        games.push(lxb_steam::Game::invented(4, "Portal 1".into(), true));
        store
            .pages
            .get_mut(&2)
            .unwrap()
            .snapshot
            .as_mut()
            .unwrap()
            .achievements
            .swap(0, 1);
        browser.sort = Sort::NameDescending;
        lattice.categories[0].entries = browser.rows(
            store.rows(&games, lxb_steam::library::Sort::InstalledFirst),
            None,
            |id| {
                games
                    .iter()
                    .find(|g| g.app_id == id)
                    .map(|game| game.installed)
            },
        );
        cursor.keep_on_trophy(&lattice, &selected);
        cursor.keep_in_bounds(&lattice);
        assert_eq!(cursor.trophy_selection(&lattice), Some(selected.clone()));
        assert_eq!(cursor.depth(), 3);
        assert!(cursor
            .opened_rows(&lattice)
            .iter()
            .any(|entry| matches!(entry, Entry::Trophy(row) if row.game() == Some(2))));
        browser.search = "Alpha".into();
        lattice.categories[0].entries = browser.rows(
            store.rows(&games, lxb_steam::library::Sort::InstalledFirst),
            None,
            |id| {
                games
                    .iter()
                    .find(|g| g.app_id == id)
                    .map(|game| game.installed)
            },
        );
        cursor.keep_on_trophy(&lattice, &selected);
        cursor.keep_in_bounds(&lattice);
        assert_eq!(
            cursor.depth(),
            1,
            "a vanished letter closes the game and its achievements"
        );
        assert!(cursor.current_entry(&lattice).unwrap().entries().is_some());
    }

    #[test]
    fn startup_counts_are_restored_without_opening_games() {
        let (send, receive) = mpsc::channel();
        let mut store = Trophies {
            account: Some(1),
            restoring: Some(receive),
            ..Default::default()
        };
        send.send((
            [(
                400,
                Progress {
                    unlocked: 7,
                    total: 15,
                    fetched_at: 1,
                },
            )]
            .into(),
            BTreeMap::new(),
        ))
        .unwrap();
        assert!(store.restore());
        let rows = store.rows(
            &[lxb_steam::Game::invented(400, "Portal".into(), false)],
            Default::default(),
        );
        assert_eq!(rows[0].comment(), Some("Steam · 7 / 15 unlocked"));
        assert!(store.pages.is_empty());
        assert!(store.watching.is_empty());
        assert!(!store.restore());
    }

    #[test]
    fn counts_are_batched_once_and_old_results_do_not_replace_fresh_progress() {
        let mut store = Trophies {
            account: Some(1),
            ..Default::default()
        };
        let games: Vec<_> = (1..=205)
            .map(|id| lxb_steam::Game::invented(id, "Game".into(), false))
            .collect();
        store.library(&games);
        assert!(store.next_progress(false).is_none());
        let (batch, request) = store.next_progress(true).unwrap();
        assert_eq!(batch.len(), 100);
        assert!(store.next_progress(true).is_none());
        store.progress.insert(
            1,
            Progress {
                unlocked: 5,
                total: 10,
                fetched_at: 20,
            },
        );
        assert!(!store.progress_heard(ProgressHeard {
            generation: 1,
            account: 2,
            request,
            result: Ok(BTreeMap::new())
        }));
        assert!(store.progress_heard(ProgressHeard {
            generation: 1,
            account: 1,
            request,
            result: Ok([(
                1,
                Progress {
                    unlocked: 1,
                    total: 10,
                    fetched_at: 10
                }
            )]
            .into())
        }));
        assert_eq!(store.progress[&1].unlocked, 5);
        for length in [100, 5] {
            let (batch, request) = store.next_progress(true).unwrap();
            assert_eq!(batch.len(), length);
            store.progress_heard(ProgressHeard {
                generation: 1,
                account: 1,
                request,
                result: Ok(BTreeMap::new()),
            });
        }
        store.library(&games);
        assert!(store.next_progress(true).is_none());
        store.reconnected();
        assert_eq!(store.next_progress(true).unwrap().0.len(), 100);
    }

    #[test]
    fn sections_are_metadata_and_refresh_does_not_insert_a_row() {
        let mut store = Trophies {
            account: Some(1),
            ..Default::default()
        };
        let mut snapshot = snapshot();
        let mut unlocked = snapshot.achievements[0].clone();
        unlocked.achieved = true;
        unlocked.apiname = "unlocked".into();
        let mut locked = unlocked.clone();
        locked.achieved = false;
        locked.hidden = false;
        locked.apiname = "locked".into();
        snapshot.achievements.splice(0..0, [unlocked, locked]);
        store.pages.insert(
            400,
            Page {
                snapshot: Some(snapshot),
                error: Some("offline".into()),
                ..page()
            },
        );
        let rows = store.rows(
            &[lxb_steam::Game::invented(400, "Portal".into(), false)],
            Default::default(),
        );
        let entries = rows[0].entries().unwrap();
        assert_eq!(entries.len(), 3);
        for (i, label) in ["Unlocked (1)", "Locked (1)", "Hidden (1)"]
            .iter()
            .enumerate()
        {
            assert_eq!(section(entries, i), Some(*label));
            assert!(matches!(
                &entries[i],
                Entry::Trophy(Row {
                    key: Key::SteamAchievement(..),
                    ..
                })
            ));
        }
    }

    #[test]
    fn known_empty_games_do_not_show_a_stats_refusal() {
        let mut store = Trophies {
            account: Some(1),
            ..Default::default()
        };
        store.progress.insert(
            400,
            Progress {
                unlocked: 0,
                total: 0,
                fetched_at: 1,
            },
        );
        store.pages.insert(
            400,
            Page {
                error: Some("code 2".into()),
                ..page()
            },
        );
        let rows = store.rows(
            &[lxb_steam::Game::invented(400, "Game".into(), false)],
            Default::default(),
        );
        assert_eq!(rows[0].comment(), Some("Steam · No achievements"));
        assert_eq!(rows[0].entries().unwrap()[0].title(), "No achievements");
    }

    #[test]
    fn late_results_cannot_replace_another_request_or_account() {
        let mut store = Trophies::default();
        store.account(Some(1));
        store.pages.insert(400, page());
        for (account, request) in [(2, 7), (1, 6)] {
            assert!(!store.heard(Heard {
                generation: 1,
                account,
                app_id: 400,
                request,
                result: Ok(snapshot())
            }));
        }
        assert!(store.heard(Heard {
            generation: 1,
            account: 1,
            app_id: 400,
            request: 7,
            result: Ok(snapshot())
        }));
        assert!(store.pages[&400].snapshot.is_some());
        store.account(None);
        store.account(Some(1));
        assert!(!store.heard(Heard {
            generation: 1,
            account: 1,
            app_id: 400,
            request: 7,
            result: Ok(snapshot())
        }));
    }
    #[test]
    fn hidden_details_are_only_exposed_by_an_explicit_press() {
        let mut store = Trophies::default();
        store.account(Some(1));
        store.pages.insert(
            400,
            Page {
                snapshot: Some(snapshot()),
                loading: false,
                ..page()
            },
        );
        let rows = store.rows(
            &[lxb_steam::Game::invented(400, "Portal".into(), false)],
            Default::default(),
        );
        assert!(!rows[0].starts_something());
        let rows = rows[0].entries().unwrap();
        let secret = &rows[0];
        assert_eq!(secret.title(), "Hidden achievement");
        assert!(!secret.comment().unwrap().contains("Spoiler"));
        assert!(secret.portrait().is_none());
        assert!(
            matches!(&secret.facts().unwrap().about, About::Listed(values) if values[0].1 == "Spoiler title")
        );
    }
    #[test]
    fn refresh_failure_preserves_the_last_good_progress() {
        let mut store = Trophies::default();
        store.account(Some(1));
        store.pages.insert(
            400,
            Page {
                snapshot: Some(snapshot()),
                ..page()
            },
        );
        assert!(store.heard(Heard {
            generation: 1,
            account: 1,
            app_id: 400,
            request: 7,
            result: Err("offline".into())
        }));
        assert_eq!(
            store.pages[&400]
                .snapshot
                .as_ref()
                .unwrap()
                .achievements
                .len(),
            1
        );
    }
    #[test]
    fn trophy_category_follows_both_integrations() {
        for retroarch in [false, true] {
            let mut categories = vec![crate::apps::Category {
                id: "games",
                title: "Games",
                icon: crate::icons::CATEGORY_GAMES,
                entries: vec![],
            }];
            let rows = || vec![status(400, "test", "Portal", "")];
            crate::apps::shelve_steam(&mut categories, rows());
            crate::apps::shelve_trophies(&mut categories, rows());
            if retroarch {
                crate::apps::shelve_retroarch(&mut categories, rows());
            }
            let ids: Vec<_> = categories.iter().map(|c| c.id).collect();
            let mut expected = vec!["games", crate::apps::steam_column()];
            if retroarch {
                expected.push(crate::apps::retroarch_column());
            }
            expected.push(COLUMN);
            assert_eq!(ids, expected);
        }
    }
    #[test]
    fn reconnect_invalidates_pending_results_and_retries_open_games() {
        let mut store = Trophies::default();
        store.account(Some(1));
        store.pages.insert(400, page());
        store.watching.insert(400);
        store.reconnected();
        assert!(store.watching.is_empty());
        assert!(!store.heard(Heard {
            generation: 1,
            account: 1,
            app_id: 400,
            request: 7,
            result: Ok(snapshot())
        }));
    }
    #[test]
    fn unknown_unlock_time_is_not_the_unix_epoch() {
        assert!(unlocked_at(0).is_none());
    }
    #[test]
    fn cursor_follows_game_and_achievement_after_a_refresh_reorders_both() {
        use crate::model::{Cursor, Lattice};
        let game = |app, children| {
            Entry::Trophy(Row {
                section: None,
                shape: None,
                installed: None,
                platform: None,
                key: Key::SteamGame(app),
                picture: None,
                entries: Some(children),
                facts: Facts {
                    title: "Game".into(),
                    comment: String::new(),
                    icon: crate::icons::STEAM.into(),
                    about: About::Listed(vec![]),
                },
            })
        };
        let mut lattice = Lattice::new(vec![crate::apps::Category {
            id: COLUMN,
            title: "Trophies",
            icon: crate::icons::CATEGORY_TROPHIES,
            entries: vec![
                game(1, vec![status(1, "a", "a", "")]),
                game(2, vec![status(2, "a", "a", ""), status(2, "b", "b", "")]),
            ],
        }]);
        let mut cursor = Cursor::for_model(&lattice);
        cursor.point_at_row(1, &lattice);
        assert!(cursor.enter(&lattice));
        cursor.point_at_row(1, &lattice);
        let selection = cursor.trophy_selection(&lattice).unwrap();
        lattice.categories[0].entries.swap(0, 1);
        lattice.categories[0].entries[0]
            .entries_vec_mut()
            .unwrap()
            .swap(0, 1);
        cursor.keep_on_trophy(&lattice, &selection);
        cursor.keep_in_bounds(&lattice);
        assert_eq!(cursor.trophy_selection(&lattice), Some(selection));
        assert_eq!(cursor.current_entry(&lattice).unwrap().title(), "b");
        assert_eq!(cursor.depth(), 1);
    }

    #[test]
    fn artwork_is_retained_for_ancestor_and_departing_columns() {
        use crate::model::{Cursor, Lattice, Standing};
        let mut store = Trophies {
            account: Some(1),
            ..Default::default()
        };
        store.pages.insert(
            400,
            Page {
                snapshot: Some(snapshot()),
                ..page()
            },
        );
        let mut rows = store.rows(
            &[lxb_steam::Game::invented(400, "Portal".into(), false)],
            Default::default(),
        );
        if let Entry::Trophy(row) = &mut rows[0].entries_vec_mut().unwrap()[0] {
            row.picture = Some("/tmp/achievement.png".into());
        }
        let lattice = Lattice::new(vec![crate::apps::Category {
            id: COLUMN,
            title: "Trophies",
            icon: crate::icons::CATEGORY_TROPHIES,
            entries: rows,
        }]);
        let mut cursor = Cursor::for_model(&lattice);
        cursor.enter(&lattice);
        while cursor.animate(1.0 / 60.0) {}
        let check = |cursor: &Cursor| {
            let columns = cursor.columns(&lattice);
            let rows: Vec<_> = columns.iter().flat_map(drawn_rows).collect();
            assert!(rows
                .iter()
                .any(|entry| matches!(entry, Entry::Trophy(row) if row.game() == Some(400))));
            assert!(rows.iter().any(|entry| entry.portrait().is_some()));
        };
        check(&cursor);
        assert!(cursor.leave());
        cursor.animate(1.0 / 60.0);
        assert!(cursor
            .columns(&lattice)
            .iter()
            .any(|c| c.standing == Standing::Leaving));
        check(&cursor);
        while cursor.animate(1.0 / 60.0) {}
        assert_eq!(cursor.depth_position(), 0.0);
        // The cursor retains the old path for re-entry; retaining its small
        // icon neighborhood until the next navigation is intentional.
        check(&cursor);
    }

    #[test]
    fn trophy_category_is_known_even_when_signed_out() {
        assert_eq!(crate::apps::known_column(COLUMN).unwrap().title, "Trophies");
    }

    #[test]
    fn corrupt_downloads_are_not_cached_as_icons() {
        assert!(!valid_icon(b"<html>Not found</html>"));
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(64, 64)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        assert!(valid_icon(bytes.get_ref()));
    }
}

/// What order the Trophies column is listed in.
///
/// Its own three rather than the Steam library's eight, and the difference is
/// the whole point of the type. The other five — recently played, play time
/// either way round, size either way round — are answers Valve's account keeps
/// about Valve's games. Nothing else in this column has them: a
/// RetroAchievements game is a set somebody has unlocked something in, and the
/// site records no size, no play time and no date for it. Offering an order
/// that silently files half the column at the bottom is the shell saying it
/// can do something it cannot, which is the one thing greying a row out exists
/// to avoid — and here there is no library that could ever answer, so there is
/// nothing to grey.
///
/// The keys are Steam's own, so a settings file written before this — and the
/// Steam column's own row, which still offers all eight — go on meaning what
/// they said.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Sort {
    /// Everything there is a copy of on this disk, then everything else, each
    /// half by name. See [`Row::installed`], which is what "a copy" means for
    /// a row that is not Valve's.
    #[default]
    InstalledFirst,
    /// Straight down the alphabet, on this disk or not.
    NameAscending,
    NameDescending,
    /// One folder per machine, and the games inside each by name.
    ///
    /// The only order here that changes the *shape* of the column rather than
    /// the order of its rows, and it is the one this column most needed: a list
    /// holding a Steam library, a PlayStation and a Game Boy is three
    /// collections sharing a screen, and the RetroArch category has always
    /// shown one machine's games as a column of their own. This is that idea a
    /// level further in.
    ///
    /// **Steam is always first** and the machines follow it by name. Not a
    /// judgement about the machines: it is the one that is not a console, it is
    /// the one whose games this shell can start, and a heading that moved about
    /// between accounts would be a list somebody has to read before they can
    /// use it.
    Platform,
}

/// Every order the column offers, in the one place that decides what its Sort
/// list looks like.
pub const SORTS: &[Sort] = &[
    Sort::InstalledFirst,
    Sort::NameAscending,
    Sort::NameDescending,
    Sort::Platform,
];

impl Sort {
    /// What the row that chooses it says. The Steam column's words for the
    /// three it shares, because they are the same three orders.
    pub fn label(self) -> &'static str {
        match self {
            Sort::InstalledFirst => crate::i18n::text("shell-installed-first"),
            Sort::NameAscending => crate::i18n::text("shell-name-a-to-z"),
            Sort::NameDescending => crate::i18n::text("shell-name-z-to-a"),
            Sort::Platform => crate::i18n::text("shell-platform"),
        }
    }

    /// What it is called in the settings file.
    pub fn key(self) -> &'static str {
        match self {
            Sort::InstalledFirst => "installed-first",
            Sort::NameAscending => "name",
            Sort::NameDescending => "name-reversed",
            Sort::Platform => "platform",
        }
    }

    /// The order of that name, or `None` for a file naming one this column does
    /// not offer — a hand-edited typo, a setting from a later version, or one
    /// of the five this column used to borrow from the Steam library and no
    /// longer does. All three are the same answer: leave the file alone and
    /// come up in the order the column has always come up in.
    pub fn from_key(key: &str) -> Option<Sort> {
        SORTS.iter().copied().find(|sort| sort.key() == key)
    }
}

/// What a game with no machine anybody here can name is filed under.
///
/// A label rather than a name: the sweep asks RetroAchievements about every
/// console number it could have, and the site hosts a few dozen this package
/// has no folder name or drawing for. A game on one of those is still that
/// account's game — see `retroachievements`, where it is listed with no console
/// on its row rather than left out — and under this order it needs somewhere to
/// stand. Naming the folder after a console nobody wrote down is the one thing
/// that must not happen.
const OTHER_PLATFORM: &str = "Other consoles";

/// The games gathered into one folder per machine, Steam first.
///
/// The order the folders come in is the whole of what is decided here, because
/// the games inside them arrive sorted by name already. Steam is pinned to the
/// top — see [`Sort::Platform`] — the machines follow it by name, and the games
/// nothing could name are last under [`OTHER_PLATFORM`], which is where a
/// catch-all belongs whatever it is called.
///
/// A row that is not a game keeps its place in the list rather than being filed
/// anywhere: there are none in this column by the time this runs, and a row
/// quietly disappearing because it did not fit the grouping is worse than one
/// standing where it always did.
fn shelved(games: Vec<Entry>) -> Vec<Entry> {
    // Insertion order is the games' order, so the first game on a machine
    // decides nothing but where its folder's mark comes from.
    let mut shelves: Vec<(Option<Platform>, Vec<Entry>)> = Vec::new();
    let mut loose = Vec::new();
    for game in games {
        let Entry::Trophy(row) = &game else {
            loose.push(game);
            continue;
        };
        let platform = row.platform.clone();
        match shelves
            .iter_mut()
            .find(|(on, _)| on.as_ref().map(|p| &p.name) == platform.as_ref().map(|p| &p.name))
        {
            Some((_, on)) => on.push(game),
            None => shelves.push((platform, vec![game])),
        }
    }
    shelves.sort_by(|(a, _), (b, _)| {
        let rank = |on: &Option<Platform>| match on {
            Some(p) if p.name == STEAM_PLATFORM => 0,
            Some(_) => 1,
            None => 2,
        };
        rank(a).cmp(&rank(b)).then_with(|| {
            let name = |on: &Option<Platform>| {
                on.as_ref().map_or(String::new(), |p| p.name.to_lowercase())
            };
            name(a).cmp(&name(b))
        })
    });
    loose.extend(shelves.into_iter().map(|(on, games)| {
        Entry::Folder(crate::apps::Folder {
            title_message: None,
            comment_message: None,
            identity: None,
            title: on.as_ref().map_or_else(
                || crate::i18n::builtin(OTHER_PLATFORM).to_string(),
                |p| p.name.clone(),
            ),
            // The count, and it is what tells this folder from a letter's —
            // see [`Position::of`], which reads the difference.
            comment: Some(crate::message!("count-games", "count" => games.len())),
            icon: Some(
                on.as_ref()
                    .map(|p| p.mark.clone())
                    .filter(|mark| crate::icons::shaped(mark))
                    .unwrap_or_else(|| crate::retroarch::mark().to_string()),
            ),
            entries: games,
            place: None,
            chosen: false,
            over_the_list: false,
            person: None,
            portrait: None,
        })
    }));
    loose
}

/// Browsing belongs to the category, not to either account provider.
pub struct Browser {
    pub sort: Sort,
    pub search: String,
}
impl Default for Browser {
    fn default() -> Self {
        Self {
            sort: crate::settings::trophies_sort().unwrap_or_default(),
            search: String::new(),
        }
    }
}
impl Browser {
    pub fn rows(
        &self,
        mut games: Vec<Entry>,
        invitation: Option<Entry>,
        steam: impl Fn(u32) -> Option<bool>,
    ) -> Vec<Entry> {
        let total = games.len();
        let needle = lxb_steam::library::sought(&self.search);
        games.retain(|g| {
            needle
                .as_ref()
                .is_none_or(|n| g.title().to_lowercase().contains(n))
        });
        // Whether there is a copy of this on the disk, asked of whichever half
        // of the column knows. A Steam row's own library answers, because that
        // answer changes while the shell is running — a download finishing is
        // a game moving to the top of this list; every other row was built
        // knowing, and says so.
        //
        // **This used to answer `true` for everything that was not Valve's**,
        // which put every RetroAchievements game in the installed half whether
        // or not there was a ROM within reach: a game the account had played
        // years ago on another machine was filed above a Steam game sitting on
        // this disk, under an order whose whole promise is that what is at the
        // top can be played.
        let installed = |entry: &Entry| -> bool {
            match entry {
                Entry::Trophy(row) => row
                    .game()
                    .and_then(&steam)
                    .or(row.installed)
                    .unwrap_or(false),
                _ => true,
            }
        };
        let names = |a: &Entry, b: &Entry| {
            a.title()
                .trim()
                .to_lowercase()
                .cmp(&b.title().trim().to_lowercase())
        };
        // Index always uses installed-first alphabetical ordering, as Steam does.
        games.sort_by(|a, b| installed(b).cmp(&installed(a)).then_with(|| names(a, b)));
        let mut letters: BTreeMap<Option<char>, Vec<Entry>> = BTreeMap::new();
        for game in &games {
            let letter = game
                .title()
                .trim()
                .chars()
                .next()
                .map(|c| c.to_ascii_uppercase())
                .filter(char::is_ascii_uppercase);
            letters.entry(letter).or_default().push(game.clone());
        }
        let folder = |title: String, icon: String, entries: Vec<Entry>, over_the_list: bool| {
            Entry::Folder(crate::apps::Folder {
                title_message: None,
                comment_message: None,
                identity: None,
                title,
                comment: None,
                icon: Some(icon),
                entries,
                place: None,
                chosen: false,
                over_the_list,
                person: None,
                portrait: None,
            })
        };
        let index = folder(
            crate::i18n::text("shell-alphabetical").into(),
            crate::icons::INDEX_MARK.into(),
            letters
                .into_iter()
                .map(|(letter, games)| {
                    folder(
                        crate::message!("count-games", "count" => games.len()),
                        crate::icons::letter_mark(letter.unwrap_or('#'))
                            .unwrap_or(crate::icons::CATEGORY_TROPHIES)
                            .into(),
                        games,
                        false,
                    )
                })
                .collect(),
            true,
        );
        games.sort_by(|a, b| {
            match self.sort {
                // A platform's own games are listed by name inside it; which
                // platform comes first is decided below, over the folders.
                Sort::InstalledFirst => installed(b).cmp(&installed(a)),
                Sort::NameAscending | Sort::Platform => names(a, b),
                Sort::NameDescending => names(b, a),
            }
            .then_with(|| names(a, b))
        });
        if self.sort == Sort::Platform {
            games = shelved(games);
        }
        let mut rows = Vec::new();
        if total > 0 {
            crate::apps::head(
                &mut rows,
                crate::apps::Searched::Trophies,
                &self.search,
                games.len(),
                total,
            );
            if !games.is_empty() {
                rows.push(index);
            }
        }
        if let Some(invitation) = invitation {
            rows.push(invitation);
        }
        rows.extend(games);
        rows
    }
}
