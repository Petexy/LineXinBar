//! Optional RetroArch helper client. No RetroAchievements networking in the desktop.
use crate::{
    apps::{About, Entry, Facts},
    dialog, menu,
    secret::Secret,
    trophies::{Key, Row},
};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc, Arc,
    },
    time::{Duration, Instant},
};

struct RequestChild(std::process::Child);
impl std::ops::Deref for RequestChild {
    type Target = std::process::Child;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl std::ops::DerefMut for RequestChild {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
impl Drop for RequestChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

static ACCOUNT_NOTE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

pub enum Stage {
    Account(String),
    Password(String, Secret),
    Waiting,
    Connected,
    Failed(String),
}
pub struct RetroAchievements {
    pub user: Option<String>,
    pub stage: Option<Stage>,
    pub error: Option<String>,
    games: Vec<Value>,
    /// The account's own games, which is the half of this that has nothing to
    /// do with what is on the disk. Kept apart from `games` rather than merged
    /// on arrival because the two are refreshed on their own clocks and either
    /// can be the only one there is — see [`RetroAchievements::rows`].
    collection: Vec<Value>,
    pages: BTreeMap<u32, Value>,
    /// The shape one console's covers are, by the name of the console.
    ///
    /// Measured off the pictures the rows will actually draw rather than
    /// tabled — the same answer a ROM shelf gets and by the same function, see
    /// [`crate::retroarch::shelf_shape`]. Worked out when the games change
    /// rather than when they are drawn: it opens eight files per console, and
    /// a column is built far less often than it is rendered.
    shapes: BTreeMap<String, f32>,
    worker: Option<mpsc::Sender<(u64, Value)>>,
    heard: Option<mpsc::Receiver<(u64, Value)>>,
    generation: Arc<AtomicU64>,
    restored: bool,
    next: Instant,
    folder: Option<PathBuf>,
    busy: usize,
    active_page: Option<u32>,
    active_library: bool,
    active_collection: bool,
    watching: BTreeSet<u32>,
    requested: BTreeMap<u32, Instant>,
    pub offer_pending: bool,
    pub resume_setup: bool,
    pub core_result: Option<bool>,
}
impl Default for RetroAchievements {
    fn default() -> Self {
        Self {
            user: None,
            stage: None,
            error: None,
            games: Vec::new(),
            collection: Vec::new(),
            pages: BTreeMap::new(),
            shapes: BTreeMap::new(),
            worker: None,
            heard: None,
            generation: Arc::new(AtomicU64::new(0)),
            restored: false,
            next: Instant::now(),
            folder: None,
            busy: 0,
            active_page: None,
            active_library: false,
            active_collection: false,
            watching: BTreeSet::new(),
            requested: BTreeMap::new(),
            offer_pending: false,
            resume_setup: false,
            core_result: None,
        }
    }
}
fn worker(
    helper: PathBuf,
    requests: mpsc::Receiver<(u64, Value)>,
    back: mpsc::Sender<(u64, Value)>,
    generation: Arc<AtomicU64>,
) {
    for (serial, mut request) in requests {
        if generation.load(Ordering::SeqCst) != serial {
            wipe(&mut request);
            continue;
        }
        let result = (|| -> Result<(), String> {
            let mut child = RequestChild(
                Command::new(&helper)
                    .arg("achievements")
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .spawn()
                    .map_err(|_| {
                        crate::i18n::text(
                            "shell-the-retroarch-achievement-helper-could-not-be-started",
                        )
                    })?,
            );
            let input = child.stdin.take().ok_or(crate::i18n::text(
                "shell-the-achievement-helper-has-no-input",
            ))?;
            let mut writer = input;
            serde_json::to_writer(&mut writer, &request)
                .map_err(|_| crate::i18n::text("shell-could-not-send-the-achievement-request"))?;
            writer
                .flush()
                .map_err(|_| crate::i18n::text("shell-could-not-send-the-achievement-request"))?;
            drop(writer);
            wipe(&mut request);
            let output = child.stdout.take().ok_or(crate::i18n::text(
                "shell-the-achievement-helper-has-no-output",
            ))?;
            let (send, receive) = mpsc::channel();
            std::thread::spawn(move || {
                for line in BufReader::new(output).lines().map_while(Result::ok) {
                    if line.len() > 16 * 1024 * 1024 {
                        break;
                    }
                    if send.send(line).is_err() {
                        break;
                    }
                }
            });
            let started = Instant::now();
            let mut done = false;
            loop {
                if generation.load(Ordering::SeqCst) != serial
                    || started.elapsed() > Duration::from_secs(600)
                {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(crate::i18n::text(
                        "shell-achievement-request-was-cancelled-or-timed-out",
                    )
                    .into());
                }
                match receive.recv_timeout(Duration::from_millis(100)) {
                    Ok(mut line) => {
                        if let Ok(event) = serde_json::from_str::<Value>(&line) {
                            done |= event["event"] == "done";
                            let _ = back.send((serial, event));
                        }
                        unsafe {
                            line.as_bytes_mut().fill(0);
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(_) => break,
                }
            }
            let status = child.wait().map_err(|_| {
                crate::i18n::text("shell-the-achievement-helper-stopped-unexpectedly")
            })?;
            if !status.success() || !done {
                return Err(if status.code() == Some(2) && !done {
                    crate::i18n::text("retroachievements-helper-too-old")
                } else {
                    crate::i18n::text("retroachievements-helper-stopped")
                }
                .into());
            }
            Ok(())
        })();
        wipe(&mut request);
        if let Err(message) = result {
            let _ = back.send((serial, json!({"event":"error","message":message})));
            let _ = back.send((serial, json!({"event":"done"})));
        }
    }
}
fn wipe(value: &mut Value) {
    match value {
        Value::String(s) => unsafe { s.as_bytes_mut().fill(0) },
        Value::Array(a) => a.iter_mut().for_each(wipe),
        Value::Object(o) => o.values_mut().for_each(wipe),
        _ => {}
    }
    std::sync::atomic::compiler_fence(Ordering::SeqCst);
}
impl Drop for RetroAchievements {
    fn drop(&mut self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
    }
}
impl RetroAchievements {
    fn start(&mut self) {
        if self.worker.is_some() {
            return;
        }
        let Some(helper) = crate::retroarch::helper().map(PathBuf::from) else {
            return;
        };
        let (send, receive) = mpsc::channel();
        let (back, heard) = mpsc::channel();
        let generation = self.generation.clone();
        std::thread::spawn(move || worker(helper, receive, back, generation));
        self.worker = Some(send);
        self.heard = Some(heard);
    }
    fn ask(&mut self, value: Value) {
        self.start();
        if self.worker.as_ref().is_some_and(|w| {
            w.send((self.generation.load(Ordering::SeqCst), value))
                .is_ok()
        }) {
            self.busy += 1;
        }
    }
    pub fn begin(&mut self) {
        self.cancel_work();
        self.stage = Some(if self.user.is_some() {
            Stage::Connected
        } else {
            Stage::Account(String::new())
        });
    }
    pub fn change_account(&mut self) {
        self.cancel_work();
        self.stage = Some(Stage::Account(String::new()));
    }
    pub fn back(&mut self) {
        self.stage = Some(match self.stage.take() {
            Some(Stage::Password(user, _)) => Stage::Account(user),
            other => {
                self.stage = other;
                return;
            }
        });
    }
    fn cancel_work(&mut self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.busy = 0;
        self.active_page = None;
        self.active_library = false;
        self.active_collection = false;
        self.requested.clear();
    }
    pub fn cancel(&mut self) {
        // Activation may have committed just before cancellation reached the worker.
        // Re-read the saved account on the next poll instead of retaining stale UI state.
        if matches!(self.stage, Some(Stage::Waiting)) {
            self.restored = false;
        }
        if matches!(
            self.stage,
            Some(Stage::Account(_) | Stage::Password(..) | Stage::Waiting | Stage::Failed(_))
        ) {
            self.cancel_work();
        }
        self.stage = None;
    }
    pub fn submit(&mut self) {
        match self.stage.take() {
            Some(Stage::Account(user)) if !user.trim().is_empty() => {
                self.stage = Some(Stage::Password(user.trim().into(), Secret::default()))
            }
            Some(Stage::Password(user, secret)) if !secret.is_empty() => {
                let request = secret
                    .as_text(|p| json!({"op":"login","user":user,"password":p}))
                    .unwrap();
                self.stage = Some(Stage::Waiting);
                self.ask(request);
            }
            other => self.stage = other,
        }
    }
    pub fn logout(&mut self) {
        self.cancel_work();
        self.stage = Some(Stage::Waiting);
        self.ask(json!({"op":"logout"}));
    }
    pub fn typing(&self) -> bool {
        matches!(self.stage, Some(Stage::Account(_) | Stage::Password(..)))
    }
    pub fn type_into(&mut self, stroke: crate::keyboard::Stroke) -> crate::steam::Typed {
        use crate::{keyboard::Stroke, steam::Typed};
        if !self.typing() {
            return Typed::Elsewhere;
        }
        match stroke {
            Stroke::ENTER => return Typed::Done { submitted: true },
            Stroke::ESCAPE => return Typed::Done { submitted: false },
            _ => {}
        }
        match self.stage.as_mut() {
            Some(Stage::Account(s)) => match stroke {
                Stroke::Char(c) if !c.is_control() && s.len() < 128 => s.push(c),
                Stroke::BACKSPACE => {
                    s.pop();
                }
                _ => {}
            },
            Some(Stage::Password(_, s)) => match stroke {
                Stroke::Char(c) if !c.is_control() => s.push(c),
                Stroke::BACKSPACE => s.pop(),
                _ => {}
            },
            _ => {}
        }
        Typed::Into
    }
    pub fn panel(&self) -> Option<(Vec<dialog::Line>, Vec<menu::Entry>)> {
        use dialog::Line;
        use menu::{Command as C, Entry as E};
        let stage = self.stage.as_ref()?;
        let mut lines = vec![Line::Heading("RetroAchievements".into())];
        let mut buttons = Vec::new();
        match stage {
            Stage::Account(user) => {
                lines.push(Line::Note(
                    crate::i18n::text("shell-enter-your-retroachievements-username").into(),
                ));
                lines.push(Line::Entry(user.clone()));
                buttons.push(E::new(
                    C::RetroAchievementsSubmit,
                    crate::i18n::text("shell-next"),
                ));
            }
            Stage::Password(user, secret) => {
                lines.push(Line::field(crate::i18n::text("shell-username"), user));
                lines.push(Line::Note(
                    crate::i18n::text("shell-enter-your-retroachievements-password").into(),
                ));
                lines.push(Line::Secret {
                    typed: secret.typed(),
                });
                buttons.push(E::new(
                    C::RetroAchievementsSubmit,
                    crate::i18n::text("shell-sign-in"),
                ));
                buttons.push(E::new(
                    C::RetroAchievementsBack,
                    crate::i18n::text("shell-back"),
                ));
            }
            Stage::Waiting => {
                lines.push(Line::Note(
                    crate::i18n::text("shell-connecting-your-account").into(),
                ));
                lines.push(Line::Waiting);
            }
            Stage::Connected => {
                lines.push(Line::field(
                    crate::i18n::text("shell-signed-in-as"),
                    self.user.as_deref().unwrap_or(""),
                ));
                lines.push(Line::Note(self.error.clone().unwrap_or_else(|| {
                    crate::i18n::text("shell-achievements-synchronize-in-the-background").into()
                })));
                buttons.push(E::new(
                    C::RetroAchievementsChange,
                    crate::i18n::text("shell-change-account"),
                ));
                buttons.push(E::new(
                    C::RetroAchievementsLogout,
                    crate::i18n::text("shell-sign-out"),
                ));
            }
            Stage::Failed(why) => {
                lines.push(Line::Note(why.clone()));
                buttons.push(E::new(
                    C::RetroAchievementsChange,
                    crate::i18n::text("shell-try-again"),
                ));
            }
        }
        buttons.push(E::new(
            C::RetroAchievementsCancel,
            if matches!(stage, Stage::Connected) {
                crate::i18n::text("shell-done")
            } else {
                crate::i18n::text("shell-cancel")
            },
        ));
        Some((lines, buttons))
    }
    pub fn poll(
        &mut self,
        available: bool,
        folder: Option<PathBuf>,
        opened: BTreeSet<u32>,
    ) -> bool {
        if !available {
            return false;
        }
        let mut changed = false;
        // Whether a shelf has to be measured again, which is not the same
        // question as whether the column changed. A shelf is measured off the
        // pictures its rows carry, so only the five answers that can bring a
        // game a picture set it: an opened page's forty badges arriving one at
        // a time changes forty rows and not one box, and re-reading every
        // console's covers for each of them would be a thousand file headers
        // read to learn nothing.
        let mut reshelve = false;
        if (self.active_library || self.active_collection)
            && opened.iter().any(|id| !self.requested.contains_key(id))
            && self.stage.is_none()
        {
            self.cancel_work();
            self.next = Instant::now();
        }
        if let Some(id) = self.active_page.filter(|id| !opened.contains(id)) {
            self.generation.fetch_add(1, Ordering::SeqCst);
            self.busy = 0;
            self.active_page = None;
            self.requested.remove(&id);
        }
        if !self.restored && self.stage.is_none() {
            self.restored = true;
            self.ask(json!({"op":"status"}));
        }
        let events: Vec<_> = self
            .heard
            .as_ref()
            .map(|r| r.try_iter().collect())
            .unwrap_or_default();
        for (serial, mut event) in events {
            if serial != self.generation.load(Ordering::SeqCst) {
                wipe(&mut event);
                continue;
            }
            match event["event"].as_str().unwrap_or("") {
                "login" => {
                    if matches!(self.stage, Some(Stage::Waiting)) {
                        self.ask(
                            json!({"op":"activate","user":event["user"],"token":event["token"]}),
                        );
                    }
                    wipe(&mut event);
                }
                "account" => {
                    self.restored = true;
                    let user = event["user"].as_str().map(str::to_string);
                    if self.user != user {
                        self.games.clear();
                        self.collection.clear();
                        self.shapes.clear();
                        self.pages.clear();
                        self.requested.clear();
                        self.user = user;
                        *ACCOUNT_NOTE.lock().unwrap_or_else(|e| e.into_inner()) = self
                            .user
                            .as_ref()
                            .map(|u| crate::message!("signed-in-as", "name" => u.as_str()));
                        self.next = Instant::now();
                        changed = true;
                        reshelve = true;
                    }
                    if matches!(self.stage, Some(Stage::Waiting)) {
                        self.stage = Some(if self.user.is_some() {
                            Stage::Connected
                        } else {
                            Stage::Account(String::new())
                        });
                    }
                    self.error = None;
                }
                "library" => {
                    self.error = None;
                    *ACCOUNT_NOTE.lock().unwrap_or_else(|e| e.into_inner()) = self
                        .user
                        .as_ref()
                        .map(|u| crate::message!("signed-in-as", "name" => u.as_str()));
                    self.games = event["games"].as_array().cloned().unwrap_or_default();
                    changed = true;
                    reshelve = true;
                }
                "game" => {
                    let game = event["game"].clone();
                    if let Some(old) = self.games.iter_mut().find(|g| g["path"] == game["path"]) {
                        *old = game;
                    } else {
                        self.games.push(game);
                    }
                    changed = true;
                    reshelve = true;
                }
                "collection" => {
                    self.error = None;
                    *ACCOUNT_NOTE.lock().unwrap_or_else(|e| e.into_inner()) = self
                        .user
                        .as_ref()
                        .map(|u| crate::message!("signed-in-as", "name" => u.as_str()));
                    self.collection = event["games"].as_array().cloned().unwrap_or_default();
                    changed = true;
                    reshelve = true;
                }
                // One game, as the sweep reaches it. The whole list follows at
                // the end of the sweep and is what settles the column; these
                // are so that a first sign-in fills in as it goes rather than
                // standing empty for a minute.
                "owned" => {
                    let game = event["game"].clone();
                    if let Some(id) = game["id"].as_u64().filter(|id| *id > 0) {
                        match self
                            .collection
                            .iter_mut()
                            .find(|g| g["id"].as_u64() == Some(id))
                        {
                            Some(old) => *old = game,
                            None => self.collection.push(game),
                        }
                        changed = true;
                        reshelve = true;
                    }
                }
                "page" => {
                    if let Some(id) = event["id"].as_u64() {
                        let page = event["page"].clone();
                        // Both halves of the column carry this game's counts,
                        // and a page is fresher than either.
                        for game in self
                            .games
                            .iter_mut()
                            .chain(self.collection.iter_mut())
                            .filter(|g| g["id"].as_u64() == Some(id))
                        {
                            if game["at"].as_u64().unwrap_or(0) > page["at"].as_u64().unwrap_or(0) {
                                continue;
                            }
                            game["at"] = page["at"].clone();
                            game["unlocked"] = page["unlocked"].clone();
                            game["hardcore"] = page["hardcore"].clone();
                            game["total"] = json!(page["achievements"].as_array().map(Vec::len));
                        }
                        self.pages.insert(id as u32, page);
                        changed = true;
                    }
                }
                "icon" => {
                    if let Some(page) = event["id"]
                        .as_u64()
                        .and_then(|id| self.pages.get_mut(&(id as u32)))
                    {
                        if let Some(a) = page["achievements"]
                            .as_array_mut()
                            .and_then(|a| a.iter_mut().find(|a| a["id"] == event["achievement"]))
                        {
                            a["picture"] = event["picture"].clone();
                            changed = true;
                        }
                    }
                }
                "error" => {
                    let why = event["message"]
                        .as_str()
                        .unwrap_or(crate::i18n::text("shell-achievements-could-not-be-loaded"))
                        .to_string();
                    self.error = Some(why.clone());
                    *ACCOUNT_NOTE.lock().unwrap_or_else(|e| e.into_inner()) = Some(why.clone());
                    if matches!(self.stage, Some(Stage::Waiting)) {
                        self.stage = Some(Stage::Failed(why));
                    }
                    changed = true;
                }
                "done" => {
                    self.busy = self.busy.saturating_sub(1);
                    if self.busy == 0 {
                        self.active_page = None;
                        self.active_library = false;
                        self.active_collection = false;
                    }
                }
                _ => {}
            }
        }
        if folder != self.folder {
            self.folder = folder;
            self.next = Instant::now();
        }
        if self.user.is_some()
            && self.busy == 0
            && matches!(self.stage, None | Some(Stage::Connected))
        {
            if let Some(id) = opened
                .iter()
                .find(|id| {
                    self.requested
                        .get(id)
                        .is_none_or(|at| at.elapsed() > Duration::from_secs(300))
                })
                .copied()
            {
                self.requested.insert(id, Instant::now());
                self.active_page = Some(id);
                self.ask(json!({"op":"page","id":id}));
            } else if Instant::now() >= self.next {
                self.next = Instant::now() + Duration::from_secs(300);
                // The account's own games are asked for whether or not this
                // machine has a ROM folder: signing in is the whole of what it
                // takes to have a Trophies column, and most people's
                // achievements were earned somewhere other than here.
                self.active_collection = true;
                self.ask(json!({"op":"collection"}));
                if let Some(folder) = &self.folder {
                    let request = json!({"op":"library","folder":folder});
                    self.active_library = true;
                    self.ask(request);
                }
            }
        }
        self.watching = opened;
        if reshelve {
            self.measure();
        }
        changed
    }

    /// Take the covers that have just landed, asking the site nothing.
    ///
    /// The only thing a picture arriving changes about this column is the path
    /// on one row. That used to be answered by [`RetroAchievements::refresh`] —
    /// through `rebuild_retroarch`, which every cover that lands calls — and a
    /// refresh is a whole `library` request: the folder walked again, every ROM
    /// fingerprinted again and an `allprogress` for every console in it, all to
    /// learn a string the poll was already holding. Measured on this machine at
    /// 2.3 s a request, one starting the instant the last one ended for as long
    /// as pictures kept coming; on a collection whose art takes minutes to come
    /// down that is hundreds of requests to a site that answers forty-three in
    /// three seconds with a 429.
    ///
    /// Answers whether anything moved — which a cover for a file this folder
    /// has not got, or one a row is already wearing, has not. The poll that
    /// carries these sets `rows` as well, so the bar is written again either
    /// way; what reads this is the regression that holds the second of those
    /// two cases to being free.
    pub fn pictured(&mut self, landed: &[(String, String)]) -> bool {
        let mut touched: BTreeSet<String> = BTreeSet::new();
        for (path, at) in landed {
            for game in self
                .games
                .iter_mut()
                .filter(|game| game["path"].as_str() == Some(path.as_str()))
            {
                if game["picture"].as_str() == Some(at.as_str()) {
                    continue;
                }
                game["picture"] = json!(at);
                if let Some(console) = game["console"].as_str().filter(|name| !name.is_empty()) {
                    touched.insert(console.to_string());
                }
            }
        }
        if touched.is_empty() {
            return false;
        }
        // Only the shelves a cover actually landed on. Measuring all of them
        // once per picture would be every console's boxes read for every cover
        // of one, which is the same reason `measure_shelves` in
        // [`crate::retroarch`] is handed a single console.
        self.measure_shelves(Some(&touched));
        true
    }
    /// What shape each console's cards are, off the pictures they will carry.
    fn measure(&mut self) {
        self.measure_shelves(None);
    }
    /// That, for the consoles in `only` — or for all of them where it is `None`.
    ///
    /// Covers first and the site's own square marks only where a console has
    /// no cover at all, so that one game libretro has never drawn does not
    /// square off a whole shelf of boxes — and so that a console where *none*
    /// of them has a cover is a shelf of squares that fill their cards rather
    /// than a shelf of squares adrift in portrait ones, which is what this
    /// replaced.
    fn measure_shelves(&mut self, only: Option<&BTreeSet<String>>) {
        let wanted = |console: &str| only.is_none_or(|only| only.contains(console));
        let mut covers: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
        let mut marks: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
        for game in self.games.iter().chain(self.collection.iter()) {
            let console = game["console"].as_str().unwrap_or_default();
            if console.is_empty() || !wanted(console) {
                continue;
            }
            // The ROM half calls its cover `picture` because that is the only
            // picture it has; the account's half tells the two apart.
            let cover = game["cover"].as_str().or_else(|| game["picture"].as_str());
            if let Some(at) = cover {
                covers.entry(console.into()).or_default().push(at.into());
            } else if let Some(at) = game["icon"].as_str() {
                marks.entry(console.into()).or_default().push(at.into());
            }
        }
        marks.retain(|console, _| !covers.contains_key(console));
        let measured = covers.into_iter().chain(marks).filter_map(|(console, at)| {
            crate::retroarch::shelf_shape(at.iter()).map(|shape| (console, shape))
        });
        match only {
            // A console named here and measured to nothing has *lost* its
            // shape — its last cover was deleted from the cache — and leaving
            // the old one behind would be this column asserting the size of
            // artwork it no longer has.
            Some(only) => {
                self.shapes.retain(|console, _| !only.contains(console));
                self.shapes.extend(measured);
            }
            None => self.shapes = measured.collect(),
        }
    }
    pub fn refresh(&mut self) {
        self.next = Instant::now();
        self.requested.clear();
    }
    pub fn removed(&mut self) {
        self.cancel_work();
        self.stage = None;
        self.user = None;
        *ACCOUNT_NOTE.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.games.clear();
        self.collection.clear();
        self.shapes.clear();
        self.pages.clear();
        self.restored = false;
        self.offer_pending = false;
        self.resume_setup = false;
        self.core_result = None;
    }
    pub fn invitation(&self) -> Entry {
        configuration_row(if let Some(user) = &self.user {
            self.error
                .clone()
                .unwrap_or_else(|| crate::message!("signed-in-as", "name" => user))
        } else {
            crate::i18n::text("shell-sign-in-to-see-achievements-from-your-retroarch-games").into()
        })
    }
    /// The achievements of one game, as the rows that open out of it.
    ///
    /// Shared by both halves of the column because an achievement is an
    /// achievement: what differs between a ROM on this disk and a game the
    /// account has played elsewhere is the row above these, not these. `issue`
    /// is the one thing only a ROM has — a file that could not be matched to a
    /// set — and is why the set is missing rather than a set of its own.
    fn achievements(&self, id: u32, total: Option<u64>, issue: Option<&str>) -> Vec<Entry> {
        let Some(page) = self.pages.get(&id).filter(|_| id != 0) else {
            return if let Some(why) = issue {
                vec![status(
                    id,
                    crate::i18n::text("shell-achievements-unavailable"),
                    why,
                )]
            } else if total == Some(0) {
                vec![status(
                    id,
                    crate::i18n::text("shell-no-achievements"),
                    crate::i18n::text("shell-this-game-has-no-official-achievements"),
                )]
            } else {
                vec![status(
                    id,
                    if self.error.is_some() {
                        crate::i18n::text("shell-achievements-unavailable")
                    } else {
                        crate::i18n::text("shell-loading-achievements")
                    },
                    self.error.as_deref().unwrap_or(crate::i18n::text(
                        "shell-asking-retroachievements-for-this-game-s-achievements",
                    )),
                )]
            };
        };
        let achievements = page["achievements"].as_array().cloned().unwrap_or_default();
        if achievements.is_empty() {
            return vec![status(
                id,
                crate::i18n::text("shell-no-achievements"),
                crate::i18n::text("shell-this-game-has-no-official-achievements"),
            )];
        }
        let unlocked = achievements
            .iter()
            .filter(|a| a["unlocked"] == true)
            .count();
        let locked = achievements.len() - unlocked;
        achievements
            .iter()
            .map(|a| {
                let state = if a["hardcore"] == true {
                    crate::i18n::text("shell-unlocked-hardcore")
                } else if a["unlocked"] == true {
                    crate::i18n::text("shell-unlocked")
                } else {
                    crate::i18n::text("shell-locked")
                };
                let description = a["description"].as_str().unwrap_or_default();
                let points = a["points"].as_u64().unwrap_or(0);
                Entry::Trophy(Row {
                    key: Key::RetroAchievement(id, a["id"].as_u64().unwrap_or(0) as u32),
                    facts: Facts {
                        title: a["title"].as_str().unwrap_or(crate::i18n::text("shell-achievement")).into(),
                        comment: crate::message!("achievement-summary", "description" => description, "points" => points, "state" => state),
                        icon: crate::icons::CATEGORY_TROPHIES.into(),
                        about: About::Listed(vec![
                            (crate::i18n::text("shell-description").into(), description.into()),
                            (crate::i18n::text("shell-points").into(), points.to_string()),
                            (crate::i18n::text("shell-status").into(), state.into()),
                        ]),
                    },
                    picture: a["picture"].as_str().map(PathBuf::from),
                    entries: None,
                    section: Some(if a["unlocked"] == true {
                        crate::message!("achievements-unlocked-count", "count" => unlocked)
                    } else {
                        crate::message!("achievements-locked-count", "count" => locked)
                    }),
                    shape: None,
                    installed: None,
                    platform: None,
                })
            })
            .collect()
    }

    /// What the account has, as the Trophies column lists it.
    ///
    /// The ROM folder first and the account's own collection behind it, and a
    /// game in both appears once — as the ROM, because that row is the one
    /// that can say which file on this disk it is and wears the box art the
    /// thumbnail server drew for it. Order beyond that is not decided here:
    /// the column sorts and searches everything it is given, whichever
    /// provider gave it — see [`crate::trophies::Browser`].
    pub fn rows(&self) -> Vec<Entry> {
        if self.user.is_none() {
            return Vec::new();
        }
        let mut rows = Vec::new();
        let mut listed = BTreeSet::new();
        // What the account's own half found for these same games. A game in
        // both halves is listed once, as the ROM — so where libretro drew
        // nothing under the *file's* name, the picture the collection found
        // under the *site's* name is one this row would otherwise throw away.
        // `Adventure Island 3 (USA).nes` and `Adventure Island III` are one
        // game to RetroAchievements and two names to a thumbnail server, and
        // the dedupe hands the ROM the row precisely because it is the half
        // that wears a picture.
        let mut theirs: BTreeMap<u32, &str> = BTreeMap::new();
        for game in &self.collection {
            let Some(id) = game["id"].as_u64().filter(|id| *id > 0) else {
                continue;
            };
            if let Some(at) = game["cover"].as_str().or_else(|| game["icon"].as_str()) {
                theirs.insert(id as u32, at);
            }
        }
        for game in &self.games {
            let id = game["id"].as_u64().unwrap_or(0) as u32;
            let path = game["path"].as_str().unwrap_or_default();
            let console = game["console"].as_str().unwrap_or_default();
            if id != 0 {
                listed.insert(id);
            }
            let progress = match (game["unlocked"].as_u64(), game["total"].as_u64()) {
                (Some(u), Some(t)) => {
                    crate::message!("achievements-unlocked-of", "unlocked" => u, "total" => t)
                }
                _ => game["issue"]
                    .as_str()
                    .unwrap_or(crate::i18n::text("shell-loading-achievement-progress"))
                    .into(),
            };
            rows.push(Entry::Trophy(Row {
                key: Key::RetroGame(id, if id == 0 { path.into() } else { String::new() }),
                facts: Facts {
                    title: game["title"]
                        .as_str()
                        .unwrap_or(crate::i18n::text("shell-game"))
                        .into(),
                    comment: format!("RetroAchievements · {console} · {progress}"),
                    icon: crate::icons::CATEGORY_TROPHIES.into(),
                    about: About::Listed(Vec::new()),
                },
                picture: game["picture"]
                    .as_str()
                    .or_else(|| theirs.get(&id).copied())
                    .map(PathBuf::from),
                entries: Some(self.achievements(
                    id,
                    game["total"].as_u64(),
                    game["issue"].as_str(),
                )),
                section: None,
                shape: self.shapes.get(console).copied(),
                // The scan found this file in the folder somebody chose, which
                // is the whole of what "installed" can mean for a ROM.
                installed: Some(true),
                platform: machine(game),
            }));
        }
        for game in &self.collection {
            let id = game["id"].as_u64().unwrap_or(0) as u32;
            if id == 0 || !listed.insert(id) {
                continue;
            }
            let Some(title) = game["title"].as_str().filter(|t| !t.is_empty()) else {
                continue;
            };
            let progress = match (game["unlocked"].as_u64(), game["total"].as_u64()) {
                (Some(u), Some(t)) => {
                    crate::message!("achievements-unlocked-of", "unlocked" => u, "total" => t)
                }
                _ => crate::i18n::text("shell-loading-achievement-progress").into(),
            };
            let console = game["console"].as_str().unwrap_or_default();
            rows.push(Entry::Trophy(Row {
                key: Key::RetroGame(id, String::new()),
                facts: Facts {
                    title: title.into(),
                    comment: if console.is_empty() {
                        format!("RetroAchievements · {progress}")
                    } else {
                        format!("RetroAchievements · {console} · {progress}")
                    },
                    icon: crate::icons::CATEGORY_TROPHIES.into(),
                    about: About::Listed(Vec::new()),
                },
                // The cover if libretro drew one, and the site's own square
                // mark where it did not. Never both: a row wears one picture,
                // and which one it is decides the shape of the card under it.
                picture: game["cover"]
                    .as_str()
                    .or_else(|| game["icon"].as_str())
                    .map(PathBuf::from),
                entries: Some(self.achievements(id, game["total"].as_u64(), None)),
                section: None,
                shape: self.shapes.get(console).copied(),
                // A game the account has played, with no ROM of it within
                // reach: there is nothing on this machine to start. A game
                // that *is* in the folder was listed above and this one was
                // dropped, so these two answers never disagree about a game.
                installed: Some(false),
                platform: machine(game),
            }));
        }
        rows
    }
}
/// The console a game is for, as the column groups by it.
///
/// Both halves answer the same way and out of the same two fields, because they
/// are the same two fields: the helper puts the console's name and the mark
/// that goes with it on every game it sends, ROM or not. `None` where it could
/// name neither — a console RetroAchievements hosts and this package has no
/// folder name for — which files the game under the column's own catch-all
/// rather than inventing a machine. See [`crate::trophies::Sort::Platform`].
fn machine(game: &Value) -> Option<crate::trophies::Platform> {
    let name = game["console"].as_str().filter(|name| !name.is_empty())?;
    Some(crate::trophies::Platform {
        name: name.to_string(),
        mark: game["glyph"]
            .as_str()
            .unwrap_or(crate::icons::CATEGORY_TROPHIES)
            .to_string(),
    })
}

fn status(id: u32, title: &str, comment: &str) -> Entry {
    Entry::Trophy(Row {
        key: Key::RetroStatus(id, title.into()),
        facts: Facts {
            title: title.into(),
            comment: comment.into(),
            icon: crate::icons::CATEGORY_TROPHIES.into(),
            about: About::Listed(Vec::new()),
        },
        picture: None,
        entries: None,
        section: None,
        shape: None,
        installed: None,
        platform: None,
    })
}
pub fn offer_configuration(
    available: bool,
    steam_games: bool,
    connected: bool,
    no_games: bool,
) -> bool {
    available && !steam_games && (!connected || no_games)
}

pub fn settings_row() -> Entry {
    let mut row = configuration_row(
        ACCOUNT_NOTE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .unwrap_or_else(|| crate::i18n::text("shell-sign-in-to-configure-achievements").into()),
    );
    if let Entry::Trophy(r) = &mut row {
        r.facts.title = "RetroAchievements".into();
    }
    row
}
pub fn configuration_row(comment: String) -> Entry {
    Entry::Trophy(Row {
        key: Key::RetroConfigure,
        facts: Facts {
            title: crate::i18n::text("shell-configure-retroachievements").into(),
            comment,
            icon: crate::icons::CATEGORY_TROPHIES.into(),
            about: About::Listed(Vec::new()),
        },
        picture: None,
        entries: None,
        section: None,
        shape: None,
        installed: None,
        platform: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn client() -> (RetroAchievements, mpsc::Sender<(u64, Value)>) {
        let (send, heard) = mpsc::channel();
        let (request, _) = mpsc::channel();
        let mut client = RetroAchievements::default();
        client.restored = true;
        client.heard = Some(heard);
        client.worker = Some(request);
        client.next = Instant::now() + Duration::from_secs(999);
        (client, send)
    }
    fn game() -> Value {
        json!({"id":12,"path":"/roms/a.nes","title":"Example","console":"NES","total":2,"unlocked":1,"hardcore":1,"picture":"/art/a.png"})
    }
    #[test]
    fn cached_counts_and_rounded_square_achievement_rows() {
        let (mut client, events) = client();
        events
            .send((0, json!({"event":"account","user":"Alice"})))
            .unwrap();
        events
            .send((0, json!({"event":"library","games":[game()]})))
            .unwrap();
        events.send((0,json!({"event":"page","id":12,"page":{"achievements":[{"id":1,"title":"First","unlocked":true,"hardcore":true,"description":"Finish","points":5},{"id":2,"title":"Second","unlocked":false}],"unlocked":1,"hardcore":1}}))).unwrap();
        assert!(client.poll(true, None, BTreeSet::new()));
        let rows = client.rows();
        assert!(rows[0].comment().unwrap().contains("1 / 2 unlocked"));
        assert_eq!(rows[0].portrait(), Some(std::path::Path::new("/art/a.png")));
        let entries = rows[0].entries().unwrap();
        assert!(
            matches!(&entries[0],Entry::Trophy(row) if row.section.as_deref()==Some("Unlocked (1)")&&matches!(row.key,Key::RetroAchievement(12,1)))
        );
        assert!(entries[0].facts().unwrap().comment.contains("Hardcore"));
        assert!(
            matches!(&entries[1],Entry::Trophy(row) if row.section.as_deref()==Some("Locked (1)"))
        );
    }
    #[test]
    fn cancelled_account_cannot_replace_current_progress() {
        let (mut client, events) = client();
        client.user = Some("Alice".into());
        client.games = vec![game()];
        client.stage = Some(Stage::Waiting);
        client.cancel();
        events
            .send((0, json!({"event":"account","user":"Bob"})))
            .unwrap();
        assert!(!client.poll(true, None, BTreeSet::new()));
        assert_eq!(client.user.as_deref(), Some("Alice"));
        assert_eq!(client.rows().len(), 1);
        events
            .send((1, json!({"event":"account","user":"Bob"})))
            .unwrap();
        client.poll(true, None, BTreeSet::new());
        assert!(client.rows().is_empty());
    }
    #[test]
    fn initial_restore_cannot_interrupt_login() {
        let (mut client, _) = client();
        let (send, receive) = mpsc::channel();
        client.worker = Some(send);
        client.restored = false;
        client.stage = Some(Stage::Waiting);
        client.poll(true, None, BTreeSet::new());
        assert!(receive.try_recv().is_err());
        assert!(matches!(client.stage, Some(Stage::Waiting)));
        client.cancel();
        client.poll(true, None, BTreeSet::new());
        assert_eq!(receive.try_recv().unwrap().1["op"], "status");
    }
    #[test]
    fn closing_connected_panel_keeps_initial_sync_running() {
        let (mut client, _) = client();
        client.stage = Some(Stage::Connected);
        client.busy = 1;
        client.active_library = true;
        let serial = client.generation.load(Ordering::SeqCst);
        client.cancel();
        assert_eq!(client.generation.load(Ordering::SeqCst), serial);
        assert_eq!(client.busy, 1);
        assert!(client.active_library);
    }
    #[test]
    fn cached_page_cannot_roll_back_fresher_library_counts() {
        let (mut client, events) = client();
        client.user = Some("Alice".into());
        let mut game = game();
        game["at"] = json!(200);
        game["unlocked"] = json!(2);
        client.games = vec![game];
        events.send((0,json!({"event":"page","id":12,"page":{"at":100,"unlocked":1,"hardcore":0,"achievements":[{},{}]}}))).unwrap();
        client.poll(true, None, BTreeSet::new());
        assert_eq!(client.games[0]["unlocked"], 2);
    }
    #[test]
    fn login_panel_masks_password_and_cancel_clears_field() {
        let (mut client, _) = client();
        client.begin();
        let (lines, _) = client.panel().unwrap();
        assert!(lines.iter().any(|l| matches!(l,dialog::Line::Note(text) if text=="Enter your RetroAchievements username.")));
        let mut osk = crate::keyboard::Osk::default();
        osk.set_controller_in_hand(false);
        osk.offer_shell_field(client.typing());
        assert!(!osk.is_open());
        for c in "Alice".chars() {
            client.type_into(crate::keyboard::Stroke::Char(c));
            osk.offer_shell_field(client.typing());
            assert!(!osk.is_open(), "a field redraw reopened the keyboard");
        }
        client.submit();
        for c in "secret".chars() {
            client.type_into(crate::keyboard::Stroke::Char(c));
            osk.offer_shell_field(client.typing());
            assert!(!osk.is_open(), "a field redraw reopened the keyboard");
        }
        let (lines, _) = client.panel().unwrap();
        assert!(lines.iter().any(|l| matches!(l,dialog::Line::Note(text) if text=="Enter your RetroAchievements password.")));
        assert!(lines.iter().any(
            |l| matches!(l,dialog::Line::Field{label,value} if label=="Username" && value=="Alice")
        ));
        assert!(lines
            .iter()
            .any(|l| matches!(l, dialog::Line::Secret { typed: 6 })));
        assert!(!format!("{lines:?}").contains("secret"));
        client.cancel();
        assert!(!client.typing());
        assert!(client.panel().is_none());
    }
    #[test]
    fn steam_games_hide_setup_even_when_search_has_no_matches() {
        let (mut client, _) = client();
        client.user = Some("Alice".into());
        client.games = vec![game()];
        let Entry::Trophy(mut steam) = client.rows().remove(0) else {
            panic!()
        };
        steam.key = Key::SteamGame(12);
        client.user = None;
        let games = vec![Entry::Trophy(steam)];
        let invitation = offer_configuration(true, !games.is_empty(), false, games.is_empty())
            .then(|| client.invitation());
        let mut browser = crate::trophies::Browser::default();
        let rows = browser.rows(games.clone(), invitation.clone(), |_| None);
        assert!(rows
            .iter()
            .any(|r| matches!(r, Entry::Trophy(row) if row.key == Key::SteamGame(12))));
        assert!(!rows
            .iter()
            .any(|r| matches!(r, Entry::Trophy(row) if row.key == Key::RetroConfigure)));
        browser.search = "no matching game".into();
        let rows = browser.rows(games, invitation, |_| None);
        assert!(!rows
            .iter()
            .any(|r| matches!(r, Entry::Trophy(row) if row.key == Key::RetroConfigure)));
        assert_eq!(settings_row().title(), "RetroAchievements");
        assert!(offer_configuration(true, false, false, true));
        assert!(!offer_configuration(false, false, false, true));
        assert!(!offer_configuration(true, false, true, false));
    }
    #[test]
    fn password_back_preserves_username_and_clears_secret() {
        let (mut client, _) = client();
        client.begin();
        for c in "Alice".chars() {
            client.type_into(crate::keyboard::Stroke::Char(c));
        }
        client.submit();
        client.type_into(crate::keyboard::Stroke::Char('x'));
        client.back();
        assert!(matches!(&client.stage, Some(Stage::Account(user)) if user == "Alice"));
        client.submit();
        assert!(
            matches!(&client.stage, Some(Stage::Password(user, secret)) if user == "Alice" && secret.is_empty())
        );
    }
    #[test]
    fn invitation_survives_without_steam_or_roms() {
        let client = RetroAchievements::default();
        let browser = crate::trophies::Browser::default();
        let invitation = offer_configuration(true, false, false, true).then(|| client.invitation());
        let rows = browser.rows(Vec::new(), invitation, |_| None);
        assert_eq!(rows.len(), 1);
        assert!(matches!(&rows[0],Entry::Trophy(row) if row.key==Key::RetroConfigure));
        let mut categories = Vec::new();
        crate::apps::shelve_trophies(&mut categories, rows);
        assert_eq!(categories[0].id, crate::trophies::COLUMN);
        assert_eq!(settings_row().title(), "RetroAchievements");
    }
    #[test]
    fn search_and_index_mix_providers_without_identity_collision() {
        let (mut client, _) = client();
        client.user = Some("Alice".into());
        client.games = vec![game()];
        let mut games = client.rows();
        let Entry::Trophy(mut steam) = games[0].clone() else {
            panic!()
        };
        steam.key = Key::SteamGame(12);
        steam.facts.title = "Another game".into();
        games.push(Entry::Trophy(steam));
        let mut browser = crate::trophies::Browser {
            sort: crate::trophies::Sort::NameAscending,
            search: String::new(),
        };
        let rows = browser.rows(games.clone(), None, |_| None);
        assert_eq!(rows[2].title(), "Another game");
        assert_eq!(rows[3].title(), "Example");
        assert_eq!(rows[1].entries().unwrap().len(), 2);
        browser.search = "EXAMPLE".into();
        let rows = browser.rows(games, None, |_| None);
        assert_eq!(rows.last().unwrap().title(), "Example");
    }
    fn owned() -> Value {
        json!({"id":3186,"title":"Tekken 6","console":"PlayStation Portable","console_id":41,"total":65,"unlocked":4,"hardcore":4,"cover":"/art/tekken6.png","icon":"/icons/131219.png"})
    }
    #[test]
    fn polish_ui_does_not_translate_protocol_keys_or_game_titles() {
        crate::i18n::set(crate::i18n::Language::Polish);
        let (mut client, events) = client();
        events
            .send((0, json!({"event":"account","user":"Alice"})))
            .unwrap();
        events
            .send((0, json!({"event":"collection","games":[owned()]})))
            .unwrap();
        assert!(client.poll(true, None, BTreeSet::new()));
        assert_eq!(client.rows().len(), 1);
        assert_eq!(client.rows()[0].title(), "Tekken 6");
        crate::i18n::set(crate::i18n::Language::British);
    }

    #[test]
    fn the_account_fills_the_column_with_no_rom_folder_at_all() {
        let (mut client, events) = client();
        events
            .send((0, json!({"event":"account","user":"Alice"})))
            .unwrap();
        events
            .send((0, json!({"event":"collection","games":[owned()]})))
            .unwrap();
        assert!(client.poll(true, None, BTreeSet::new()));
        let rows = client.rows();
        assert_eq!(rows.len(), 1, "a signed-in account is a column of its own");
        assert_eq!(rows[0].title(), "Tekken 6");
        assert!(rows[0]
            .comment()
            .unwrap()
            .contains("PlayStation Portable · 4 / 65 unlocked"));
        assert_eq!(
            rows[0].portrait(),
            Some(std::path::Path::new("/art/tekken6.png")),
            "the cover, not the site's little square"
        );
        // It opens, and what is behind it is asked for by the same identity a
        // matched ROM would have used.
        assert!(
            matches!(&rows[0], Entry::Trophy(row) if row.key == Key::RetroGame(3186, String::new()))
        );
        assert_eq!(
            rows[0].entries().unwrap()[0].title(),
            "Loading achievements…"
        );
    }
    #[test]
    fn a_rom_that_has_been_played_is_one_row_and_it_is_the_rom() {
        let (mut client, _) = client();
        client.user = Some("Alice".into());
        client.games = vec![game()];
        let mut played = owned();
        played["id"] = json!(12);
        played["title"] = json!("Example, as the site names it");
        client.collection = vec![played, owned()];
        let rows = client.rows();
        assert_eq!(rows.len(), 2);
        // The ROM keeps the row, because it is the one that knows which file
        // on this disk it is.
        assert_eq!(rows[0].title(), "Example");
        assert_eq!(rows[0].portrait(), Some(std::path::Path::new("/art/a.png")));
        assert_eq!(rows[1].title(), "Tekken 6");
    }
    #[test]
    fn the_rom_that_keeps_the_row_keeps_the_account_s_picture_too() {
        let (mut client, _) = client();
        client.user = Some("Alice".into());
        // A file libretro drew nothing for under the name it has here: the
        // thumbnail server has `Adventure Island III (Europe) (Proto)` and the
        // folder has `Adventure Island 3 (USA).nes`.
        let mut rom = game();
        rom["picture"] = Value::Null;
        client.games = vec![rom];
        let mut played = owned();
        played["id"] = json!(12);
        played["title"] = json!("Example, as the site names it");
        client.collection = vec![played];
        let rows = client.rows();
        assert_eq!(rows.len(), 1, "one game, one row, and it is the ROM");
        assert_eq!(rows[0].title(), "Example");
        assert_eq!(
            rows[0].portrait(),
            Some(std::path::Path::new("/art/tekken6.png")),
            "the cover the account's half found, rather than nothing at all"
        );
        // And a ROM libretro *has* drawn keeps its own, which is the picture of
        // this dump rather than of the game in general.
        client.games = vec![game()];
        assert_eq!(
            client.rows()[0].portrait(),
            Some(std::path::Path::new("/art/a.png"))
        );
        // The site's own square mark stands in where there is no cover either,
        // on the same terms the account's own rows take it.
        client.games = vec![{
            let mut rom = game();
            rom["picture"] = Value::Null;
            rom
        }];
        client.collection[0]["cover"] = Value::Null;
        assert_eq!(
            client.rows()[0].portrait(),
            Some(std::path::Path::new("/icons/131219.png"))
        );
    }
    #[test]
    fn a_cover_that_lands_reaches_the_row_without_asking_the_site() {
        let (mut client, _) = client();
        client.user = Some("Alice".into());
        let mut rom = game();
        rom["picture"] = Value::Null;
        client.games = vec![rom];
        assert_eq!(client.rows()[0].portrait(), None);

        // The picture the RetroArch half has just fetched, handed straight
        // over. Nothing may be sent to the helper for it: a `library` request
        // walks the folder and asks the site about every console in it, to
        // learn the string already in this call.
        assert!(client.pictured(&[("/roms/a.nes".into(), "/art/a.png".into())]));
        assert_eq!(client.busy, 0, "a cover is not a reason to ask anybody");
        assert_eq!(
            client.rows()[0].portrait(),
            Some(std::path::Path::new("/art/a.png"))
        );
        // The same one again has changed nothing, and a rebuild of the whole
        // bar is not free.
        assert!(!client.pictured(&[("/roms/a.nes".into(), "/art/a.png".into())]));
        // Nor is a cover for a file this folder has not got.
        assert!(!client.pictured(&[("/roms/elsewhere.nes".into(), "/art/b.png".into())]));
    }

    #[test]
    fn a_cover_landing_measures_its_own_shelf_and_leaves_the_others() {
        let at = std::env::temp_dir().join(format!("lxb-ra-landed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&at);
        let umd = at.join("tekken6.png");
        let jewel = at.join("tekken2.png");
        picture(&umd, 512, 882);
        picture(&jewel, 560, 545);

        let (mut client, _) = client();
        client.user = Some("Alice".into());
        client.collection = vec![{
            let mut played = owned();
            played["cover"] = json!(umd.display().to_string());
            played
        }];
        let mut rom = game();
        rom["picture"] = Value::Null;
        rom["console"] = json!("PlayStation");
        rom["path"] = json!("/roms/tekken2.bin");
        client.games = vec![rom];
        client.measure();
        let umd_shape = client.shapes.get("PlayStation Portable").copied();
        assert!(umd_shape.is_some(), "the console with a cover was measured");
        assert_eq!(client.shapes.get("PlayStation"), None);

        assert!(client.pictured(&[("/roms/tekken2.bin".into(), jewel.display().to_string())]));
        let jewel_shape = client.shapes.get("PlayStation").copied();
        assert!(
            jewel_shape.is_some_and(|shape| (shape - 1.03).abs() < 0.03),
            "the shelf the cover landed on is a jewel case now: {jewel_shape:?}"
        );
        assert_eq!(
            client.shapes.get("PlayStation Portable").copied(),
            umd_shape,
            "and the shelves it did not land on were left exactly as they were"
        );
        std::fs::remove_dir_all(&at).ok();
    }

    #[test]
    fn badges_arriving_do_not_measure_a_single_shelf() {
        let at = std::env::temp_dir().join(format!("lxb-ra-badges-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&at);
        let umd = at.join("tekken6.png");
        picture(&umd, 512, 882);

        let (mut client, events) = client();
        client.user = Some("Alice".into());
        let mut played = owned();
        played["cover"] = json!(umd.display().to_string());
        events
            .send((0, json!({"event":"collection","games":[played]})))
            .unwrap();
        assert!(client.poll(true, None, BTreeSet::new()));
        let shape = client.shapes.get("PlayStation Portable").copied();
        assert!(shape.is_some());

        // A page and its badges change rows and no boxes at all. Taking the
        // cover away underneath proves the shelves were not read again: a
        // measure here would drop the shape with the file.
        events.send((0,json!({"event":"page","id":3186,"page":{"achievements":[{"id":1,"title":"First"}],"unlocked":0,"hardcore":0}}))).unwrap();
        events
            .send((
                0,
                json!({"event":"icon","id":3186,"achievement":1,"picture":"/badges/1.png"}),
            ))
            .unwrap();
        std::fs::remove_dir_all(&at).ok();
        assert!(client.poll(true, None, BTreeSet::new()));
        assert_eq!(
            client.shapes.get("PlayStation Portable").copied(),
            shape,
            "an achievement's badge is not a reason to read every console's boxes"
        );
    }

    /// One PNG of the given size, for the shape it is measured at.
    fn picture(at: &std::path::Path, w: u32, h: u32) {
        std::fs::create_dir_all(at.parent().unwrap()).unwrap();
        image::DynamicImage::ImageRgba8(image::ImageBuffer::from_pixel(
            w,
            h,
            image::Rgba([0u8, 0, 0, 255]),
        ))
        .save(at)
        .unwrap();
    }

    #[test]
    fn a_console_is_drawn_at_the_shape_of_its_own_boxes() {
        let at = std::env::temp_dir().join(format!("lxb-ra-shape-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&at);
        // A UMD case, and the site's own square mark for a game libretro has
        // never drawn a cover for.
        let umd = at.join("tekken6.png");
        let mark = at.join("131219.png");
        picture(&umd, 512, 882);
        picture(&mark, 96, 96);

        let (mut client, _) = client();
        client.user = Some("Alice".into());
        let mut covered = owned();
        covered["cover"] = json!(umd.display().to_string());
        let mut bare = owned();
        bare["id"] = json!(7514);
        bare["title"] = json!("Tekken 4");
        bare["console"] = json!("PlayStation 2");
        bare["cover"] = Value::Null;
        bare["icon"] = json!(mark.display().to_string());
        client.collection = vec![covered, bare];
        client.measure();

        let shape = |title: &str| {
            client.rows().into_iter().find_map(|row| match row {
                Entry::Trophy(row) if row.facts.title == title => Some(row.shape),
                _ => None,
            })
        };
        assert_eq!(
            shape("Tekken 6"),
            Some(Some(0.58)),
            "a console with covers is drawn at the shape of them"
        );
        assert_eq!(
            shape("Tekken 4"),
            Some(Some(1.0)),
            "and one with none at all fills its cards with what it does have, \
             rather than standing a square in the middle of a tall card"
        );
        let _ = std::fs::remove_dir_all(&at);
    }

    /// "Installed first" means there is something on this disk to start.
    ///
    /// The regression, reported off a screenshot: every RetroAchievements game
    /// was in the installed half, because the column answered `true` for
    /// everything that was not Valve's. Adventures in the Magic Kingdom stood
    /// at the top of a list whose promise is "these are the ones you can play",
    /// with no ROM of it anywhere the emulator could reach.
    #[test]
    fn installed_first_means_a_rom_is_in_the_folder() {
        let (mut client, _) = client();
        client.user = Some("Alice".into());
        // A ROM the scan found in the chosen folder, named so that every other
        // order would put it last.
        let mut on_disk = game();
        on_disk["id"] = json!(99);
        on_disk["title"] = json!("Zulu");
        client.games = vec![on_disk];
        // And a game played somewhere else, named so that it would come first
        // under any order but this one.
        let mut played = owned();
        played["title"] = json!("Adventures in the Magic Kingdom");
        client.collection = vec![played];

        let listed = |sort| {
            crate::trophies::Browser {
                sort,
                search: String::new(),
            }
            .rows(client.rows(), None, |_| None)
            .into_iter()
            .filter(|row| matches!(row, Entry::Trophy(row) if row.entries.is_some()))
            .map(|row| row.title().to_string())
            .collect::<Vec<_>>()
        };
        assert_eq!(
            listed(crate::trophies::Sort::InstalledFirst),
            ["Zulu", "Adventures in the Magic Kingdom"],
            "the ROM in the folder comes first; a game with no copy here does not"
        );
        // And the orders that are only about the name are not touched by it.
        assert_eq!(
            listed(crate::trophies::Sort::NameAscending),
            ["Adventures in the Magic Kingdom", "Zulu"]
        );
        assert_eq!(
            listed(crate::trophies::Sort::NameDescending),
            ["Zulu", "Adventures in the Magic Kingdom"]
        );
    }

    /// Sorted by platform, the column is a folder per machine with Steam at the
    /// top — the RetroArch category's shape, a level further in.
    #[test]
    fn platform_shelves_the_column_by_machine_with_steam_first() {
        let (mut client, _) = client();
        client.user = Some("Alice".into());
        let mut nes = owned();
        nes["id"] = json!(1446);
        nes["title"] = json!("Super Mario Bros.");
        nes["console"] = json!("Nintendo Entertainment System");
        nes["glyph"] = json!("lxb:console-nes");
        // A console the site hosts and this package has no name for.
        let mut nameless = owned();
        nameless["id"] = json!(9001);
        nameless["title"] = json!("Something Else");
        nameless["console"] = json!("");
        client.collection = vec![owned(), nes, nameless];

        // One Steam row, standing in for the other half of the column.
        let steam = Entry::Trophy(Row {
            key: Key::SteamGame(400),
            facts: Facts {
                title: "Half-Life".into(),
                comment: String::new(),
                icon: crate::icons::STEAM.into(),
                about: About::Listed(Vec::new()),
            },
            picture: None,
            entries: Some(Vec::new()),
            section: None,
            shape: None,
            installed: None,
            platform: Some(crate::trophies::Platform::steam()),
        });

        let mut games = vec![steam];
        games.extend(client.rows());
        let rows = crate::trophies::Browser {
            sort: crate::trophies::Sort::Platform,
            search: String::new(),
        }
        .rows(games, None, |_| Some(true));
        let shelves: Vec<_> = rows
            .iter()
            .filter_map(|row| match row {
                Entry::Folder(folder) if folder.comment.is_some() => {
                    Some((folder.title.as_str(), folder.entries.len()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            shelves,
            [
                ("Steam", 1),
                ("Nintendo Entertainment System", 1),
                ("PlayStation Portable", 1),
                ("Other consoles", 1),
            ],
            "Steam first, the machines by name, and what nothing could name last"
        );
        // Every game is on a shelf and none is left standing in the column.
        assert!(!rows
            .iter()
            .any(|row| matches!(row, Entry::Trophy(row) if row.entries.is_some())));
        // The folders are told apart from the Alphabetical index's letters, so
        // a cursor standing on one comes back to it rather than to a letter.
        let of = |row: &Entry| crate::trophies::Position::of(row);
        assert_eq!(
            rows.iter()
                .filter_map(of)
                .filter(|p| matches!(p, crate::trophies::Position::Platform(_)))
                .count(),
            4
        );
    }

    #[test]
    fn a_console_this_machine_cannot_name_still_lists_its_games() {
        let (mut client, _) = client();
        client.user = Some("Alice".into());
        let mut elsewhere = owned();
        elsewhere["console"] = json!("");
        client.collection = vec![elsewhere];
        let rows = client.rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].comment().unwrap(),
            "RetroAchievements · 4 / 65 unlocked",
            "a console with no name here must not leave a gap in the line"
        );
    }
    #[test]
    fn a_game_the_site_would_not_name_is_not_listed() {
        let (mut client, _) = client();
        client.user = Some("Alice".into());
        let mut nameless = owned();
        nameless["title"] = json!("");
        client.collection = vec![nameless];
        assert!(client.rows().is_empty());
    }
    #[test]
    fn known_empty_and_unmatched_roms_have_explanations() {
        let (mut client, _) = client();
        client.user = Some("Alice".into());
        let mut empty = game();
        empty["total"] = json!(0);
        empty["unlocked"] = json!(0);
        let mut unmatched = game();
        unmatched["id"] = json!(0);
        unmatched["issue"] = json!("This ROM is not recognized by RetroAchievements");
        client.games = vec![empty, unmatched];
        let rows = client.rows();
        assert_eq!(rows[0].entries().unwrap()[0].title(), "No achievements");
        assert!(rows[1].entries().unwrap()[0]
            .comment()
            .unwrap()
            .contains("not recognized"));
    }
}
