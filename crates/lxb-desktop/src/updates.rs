//! The Settings-facing half of updates. All IPC and process discovery happens
//! on a worker. Closing a panel only closes the view, never the transaction.
//!
//! The page is written for somebody who wants to press Update and see a bar,
//! which is what a console gives them: a check that ends in a count and one
//! button, a bar that fills while the machine does it, a word about a
//! restart, and nothing else unless they go looking. It was first written as
//! a report — "Review before continuing. Native tools may still ask
//! questions.", a line of prose per source, four buttons with "Continue with
//! these sources" as the verb, and a Details view that paged a machine's
//! five hundred pending packages one line at a time, fifty-nine pages of
//! them. The user's word for that was *overcomplicated*, and it was.
//!
//! The tools run unattended — Update now was the confirmation, and the
//! coordinator hands each tool the flag that says so, see
//! [`lxb_updates::discovery::steps`] — but what a tool asks anyway still has
//! to be answered: a conffile dpkg cannot decide about, or credentials for a
//! private repository. So a `[Y/n]` at the end of its output becomes a Yes and
//! a No on the panel rather than a keyboard, and a password prompt opens the
//! field itself. The list of what is waiting lives in the Settings column as
//! a folder — see [`rows`] — which is how this shell shows a long list:
//! scrolled with a held thumb, not paged with a button.
//!
//! And what the tools *said* is shown as what it is: a terminal. **Full
//! output** is a frame of [`lxb_updates::COLUMNS`] columns in a fixed-width
//! face with the whole transcript in it — every line the coordinator kept,
//! which is the whole of an ordinary run — following the tail while the tools
//! run and scrolled with Left and Right. It was first a ten-line page of
//! wrapped prose over a sixty-line tail, which showed neither the output nor
//! the terminal it came from.
//!
//! One more thing the panel does is get out of the way. A root step goes
//! through `pkexec`, and `polkitd` asks this session's own agent for the
//! password — a panel of its own, on the same centred glass. The running
//! panel is *set aside* for it rather than dismissed, and comes back when
//! the question is answered — see [`Updates::set_aside`] and the shell's
//! `sync_polkit`. Before that the question waited behind the panel until the
//! person happened to hide it, which read as an update that never started.
use crate::{dialog::Line, keyboard::Stroke, menu, secret::Secret, settings::UpdateValue};
use lxb_updates::{Phase, Request, Response, Snapshot, Source, SourceId, COLUMNS};
use std::sync::{mpsc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// One source as the Settings column shows it: a row that acts, with the
/// state of the source in a few words under it, and — when the source
/// listed what it would change — the list itself, for the Available updates
/// folder.
#[derive(Clone, PartialEq, Eq)]
pub struct Row {
    pub id: SourceId,
    pub note: String,
    pub items: Vec<lxb_updates::Item>,
}

/// The sources as the coordinator last described them, which is what the
/// column's rows are derived from.
static SOURCES: OnceLock<Mutex<Vec<Source>>> = OnceLock::new();

fn sources() -> Vec<Source> {
    SOURCES
        .get_or_init(|| Mutex::new(vec![]))
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
}

/// The rows of the Updates page, one a source.
///
/// Before the coordinator has answered — which is before it has been started,
/// on a fresh session — the two sources every machine has are offered, so the
/// page is never empty; the rest appear when they are found.
pub fn rows() -> Vec<Row> {
    let known = sources();
    if known.is_empty() {
        return [SourceId::System, SourceId::Firmware]
            .into_iter()
            .map(|id| Row {
                id,
                note: crate::i18n::text("shell-not-checked-yet").into(),
                items: vec![],
            })
            .collect();
    }
    known
        .iter()
        .filter(|source| source.id != SourceId::Aur)
        .map(|source| Row {
            id: source.id,
            note: summary(source),
            items: if source.listed && source.error.is_none() {
                source.items.clone()
            } else {
                vec![]
            },
        })
        .collect()
}

/// What every source adds up to, for the note under "Update everything" —
/// or nothing, before anything has been checked.
pub fn overall() -> Option<String> {
    let known = sources();
    let tally = Tally::of(known.iter());
    (!tally.unchecked).then(|| tally.headline())
}

enum Work {
    Request(Request),
    Input { job: u64, text: Secret },
}
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum View {
    Job,
    /// The transcript, in a terminal frame. See [`Updates::show_output`].
    Output,
    History,
    Preferences,
}

/// Where the terminal frame's window is in the transcript.
///
/// `Following` is not a number because the thing it follows keeps growing:
/// a frame opened on the last rows of a running transaction has to go on
/// showing the last rows as the tool says more, and a number would have
/// pinned it to the rows that were last when it opened. It follows until
/// somebody scrolls, and follows again when they scroll back to the end.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scroll {
    Following,
    /// The index of the first row on show.
    At(usize),
}

impl Scroll {
    /// The first row on show, of `count` rows in a frame `rows` tall.
    fn top(self, count: usize, rows: usize) -> usize {
        let last = count.saturating_sub(rows);
        match self {
            Self::Following => last,
            Self::At(at) => at.min(last),
        }
    }
}

/// How many rows the terminal frame holds.
///
/// Twenty, which with the panel's heading, the foot and two buttons stands
/// inside a 720p display (tested), and is a screen of a terminal to read at
/// a time. Not more: a frame that ran to the bottom of a television would
/// put the buttons under it out of reach of the eye reading the top.
const TERMINAL_ROWS: usize = 20;

pub struct Updates {
    send: mpsc::Sender<Work>,
    receive: mpsc::Receiver<Result<Response, String>>,
    pending: usize,
    /// A press is on its way to the coordinator and nothing has come back.
    ///
    /// While this is up the panel shows a wait and offers nothing but Close:
    /// the snapshot it holds is the one from *before* the press — a review
    /// with its Update button, say — and the coordinator answering a second
    /// Update with "the review changed" is a press that was let through and
    /// refused, where the rule is that a press made while the last one is
    /// being answered is spent, not acted on. Status polls do not raise
    /// this; only what the user did.
    awaiting: bool,
    pub snapshot: Snapshot,
    pub open: bool,
    /// The panel is open but off the screen, because `polkitd` asked this
    /// session to prove something and the question took its place. It comes
    /// back when the question is answered. See [`Self::set_aside`].
    pub aside: bool,
    pub view: View,
    /// Where the terminal frame's window is in the transcript.
    scroll: Scroll,
    pub typing: bool,
    typed: Secret,
    /// The line of output a Yes or No was the answer to. A tool that echoes
    /// the answer leaves its question on the screen with a `y` after it,
    /// still reading as a question; this is how the panel knows it has been
    /// answered and does not offer the same Yes twice. Matched as a prefix
    /// of the last line, for that echo.
    answered: Option<(u64, String)>,
    history: Vec<Snapshot>,
    /// A job has ended since Recent updates was last asked for, so the list
    /// on show is one attempt short. Asked for again while that view is up.
    history_stale: bool,
    daily: bool,
    output_job: Option<u64>,
    output_history: bool,
    log: lxb_updates::process::Transcript,
    log_next: u64,
    log_total: u64,
    /// The whole of the open job's transcript is here and the job has
    /// stopped writing, so there is nothing left to ask for.
    ///
    /// Without this the last thing a tool said before it finished could sit
    /// on the coordinator's disk unread: the frame polls while the job is
    /// running, and the moment it stops running the poll stops too — but
    /// `log_total` is only as new as the last chunk that came back, so
    /// "everything I know about is here" is not the same as "everything is
    /// here". One more ask after the job stops settles it.
    log_settled: bool,
    log_error: Option<String>,
    next_events: Instant,
    pub events: Vec<lxb_updates::JobEvent>,
    pub notifications: std::collections::HashMap<u32, lxb_updates::JobEvent>,
    seen_events: std::collections::HashSet<String>,
    pending_events: std::collections::HashSet<String>,
    error: Option<String>,
    next: Instant,
    next_check: Instant,
    pub buttons: Vec<menu::Command>,
}

impl Default for Updates {
    fn default() -> Self {
        let (send, work) = mpsc::channel();
        let (done, receive) = mpsc::channel();
        std::thread::spawn(move || {
            let helper = std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|p| p.join("lxb-updates")))
                .filter(|p| p.is_file())
                .or_else(|| lxb_updates::process::find("lxb-updates"));
            for work in work {
                let result = match helper.as_deref() {
                    None => Err(crate::i18n::text("updates-helper-missing").into()),
                    Some(helper) => match work {
                        Work::Request(request) => lxb_updates::service::request(helper, &request)
                            .map_err(|e| e.to_string()),
                        Work::Input { job, text } => {
                            lxb_updates::service::input(helper, job, |stream| text.hand_to(stream))
                                .map_err(|e| e.to_string())
                        }
                    },
                };
                if done.send(result).is_err() {
                    break;
                }
            }
        });
        Self::over(send, receive)
    }
}

impl Updates {
    /// The state machine over a worker of the caller's choosing — the one
    /// [`Default`] spawns, or in a test the two ends of the channel held by
    /// the test itself, so what was asked can be read and answered without a
    /// coordinator ever being started.
    fn over(send: mpsc::Sender<Work>, receive: mpsc::Receiver<Result<Response, String>>) -> Self {
        Self {
            send,
            receive,
            pending: 0,
            awaiting: false,
            snapshot: Snapshot::default(),
            open: false,
            aside: false,
            view: View::Job,
            scroll: Scroll::Following,
            typing: false,
            typed: Secret::default(),
            answered: None,
            history: vec![],
            history_stale: false,
            daily: true,
            output_job: None,
            output_history: false,
            log: lxb_updates::process::Transcript::with_room(1_000_000),
            log_next: 0,
            log_total: 0,
            log_settled: false,
            log_error: None,
            next_events: Instant::now(),
            events: vec![],
            notifications: Default::default(),
            seen_events: Default::default(),
            pending_events: Default::default(),
            error: None,
            next: Instant::now(),
            next_check: Instant::now(),
            buttons: vec![],
        }
    }

    fn request(&mut self, request: Request) {
        let action = !matches!(
            request,
            Request::Status
                | Request::Output { .. }
                | Request::Events
                | Request::Delivered { .. }
                | Request::Acknowledge { .. }
        );
        if action {
            self.error = None;
        }
        if self.send.send(Work::Request(request)).is_ok() {
            self.pending += 1;
            self.awaiting |= action;
        } else {
            self.error = Some(crate::i18n::text("shell-the-update-client-worker-stopped").into());
        }
    }
    pub fn press(&mut self, action: UpdateValue) {
        self.open = true;
        self.aside = false;
        self.scroll = Scroll::Following;
        self.typing = false;
        self.typed = Secret::default();
        self.error = None;
        self.view = match action {
            UpdateValue::History => View::History,
            UpdateValue::Preferences => View::Preferences,
            _ => View::Job,
        };
        match action {
            UpdateValue::History => {
                self.history_stale = false;
                self.request(Request::History)
            }
            UpdateValue::Preferences => self.request(Request::Preferences),
            // A job under way — or a check the daily poll sent a moment ago
            // and has not heard back from — is what this press wants to see,
            // not something to start beside: the coordinator would refuse a
            // second check as "already active", and the user would read a
            // refusal of the thing they asked for.
            _ if self.snapshot.busy() || self.awaiting => self.request(Request::Status),
            UpdateValue::Everything | UpdateValue::Check => {
                self.request(Request::Check { selected: vec![] })
            }
            UpdateValue::Source(id) => self.request(Request::Check { selected: vec![id] }),
        }
    }
    pub fn install(&mut self) {
        self.error = None;
        self.request(Request::Install {
            job: self.snapshot.job,
        });
    }
    pub fn cancel_check(&mut self) {
        self.request(Request::CancelCheck {
            job: self.snapshot.job,
        });
    }
    pub fn restart(&mut self) {
        self.request(Request::Restart {
            job: self.snapshot.job,
        });
    }
    pub fn toggle_daily(&mut self) {
        self.request(Request::SetDailyCheck(!self.daily));
    }
    /// Put the panel away.
    ///
    /// Not while it is set aside for an authorisation: the panel being taken
    /// down then is `polkitd`'s, and the shell takes every panel down through
    /// the one door. The panel that was set aside is still open, and comes
    /// back when the question is over — see [`Self::set_aside`].
    pub fn hide(&mut self) {
        if self.aside {
            return;
        }
        self.open = false;
        self.typing = false;
        self.typed = Secret::default();
        self.buttons.clear();
    }
    /// Take the panel off the screen for a question `polkitd` has asked,
    /// without closing it.
    ///
    /// A root step goes through `pkexec`, and the password for it is asked
    /// by this session's own polkit agent, on the same centred glass this
    /// panel is drawn on. The shell puts one panel up at a time, so the
    /// question either waits behind this one or takes its place — and it
    /// has to take its place, because what this panel would show meanwhile
    /// is "Working…" over a tool that is waiting for the very password the
    /// question is asking for. Before this the question waited until the
    /// person happened to hide the panel, and an update that never got past
    /// its lights was what they saw instead.
    ///
    /// The field is closed with the panel: whatever was being typed was for
    /// the tool, and the tool is not what is asking now. The panel keeps its
    /// view and its place in the transcript, so a person reading the output
    /// finds it where they left it.
    pub fn set_aside(&mut self) {
        self.aside = true;
        self.typing = false;
        self.typed = Secret::default();
        self.buttons.clear();
    }
    pub fn edit(&mut self) {
        self.typing = true;
        self.typed = Secret::default();
    }
    /// Answer the question at the end of the tool's output with a `y` or an
    /// `n`, which is what every package manager's confirmation takes, so
    /// that "Proceed with installation? [Y/n]" is a button and not a trip
    /// through the on-screen keyboard for one letter.
    pub fn answer(&mut self, yes: bool) {
        let mut text = Secret::default();
        text.push(if yes { 'y' } else { 'n' });
        self.answered = self.last_line().map(|line| {
            (
                lxb_updates::prompt::position(&self.snapshot),
                line.to_owned(),
            )
        });
        self.send_input(text);
    }
    /// Open the transcript, in its terminal frame, on its last rows —
    /// following the tool while one runs, because what it is saying *now* is
    /// what somebody pressing "Full output" came to read.
    pub fn show_output(&mut self) {
        self.open_log(self.snapshot.job, false);
    }
    pub fn open_log(&mut self, job: u64, history: bool) {
        self.open = true;
        self.output_history = history;
        self.view = View::Output;
        self.scroll = Scroll::Following;
        self.output_job = Some(job);
        self.log = lxb_updates::process::Transcript::with_room(1_000_000);
        self.log_next = 0;
        self.log_total = 0;
        self.log_settled = false;
        self.log_error = None;
        if job != 0 && (history || self.snapshot.phase != Phase::Reviewing) {
            self.request(Request::Output { job, offset: 0 });
        } else {
            for source in self.selected().cloned().collect::<Vec<_>>() {
                self.log
                    .push(&format!("— {} —\n{}\n", source.id.name(), source.note));
                for item in source.items {
                    self.log.push(&format!("{}  {}\n", item.name, item.detail));
                }
                for item in source.excluded {
                    self.log
                        .push(&format!("Excluded: {} — {}\n", item.name, item.detail));
                }
            }
        }
    }
    pub fn output_back(&mut self) {
        if self.output_history {
            self.press(UpdateValue::History);
        } else {
            self.show_overview();
        }
    }
    pub fn follow_output(&mut self) {
        self.scroll = Scroll::Following;
    }
    /// The lines the terminal frame is a window on.
    ///
    /// The durable transcript once any of it is here — that is the whole of
    /// what the tools said, preamble and all, and it is what Full output is
    /// for. Before the first chunk has come back there is still something to
    /// show: the status snapshot carries the last of the live output, and a
    /// frame opened on a running job that sat blank until a round trip
    /// finished was a press that appeared to do nothing. The transcript
    /// replaces it the moment it lands.
    ///
    /// A job out of Recent updates has no snapshot of its own — the
    /// snapshot belongs to whatever is running now, which is a different
    /// job — so it waits for its own bytes rather than borrowing someone
    /// else's.
    fn output_lines(&self) -> Vec<String> {
        if let Some(error) = &self.log_error {
            return vec![error.clone()];
        }
        let transcript = self.log.lines();
        if !transcript.is_empty() {
            return transcript;
        }
        if self.output_history || self.output_job != Some(self.snapshot.job) {
            return vec![];
        }
        self.snapshot.output.clone()
    }
    fn stale_attention_notifications(&self) -> Vec<u32> {
        self.notifications
            .iter()
            .filter_map(|(id, event)| {
                (event.attention
                    && (!self.pending_events.contains(&event.id)
                        || self.snapshot.job != event.job
                        || self.snapshot.phase != Phase::Running))
                    .then_some(*id)
            })
            .collect()
    }
    fn shows_job(&self, job: u64) -> bool {
        self.open
            && !self.aside
            && self.snapshot.job == job
            && (self.view == View::Job
                || (self.view == View::Output && self.output_job == Some(job)))
    }
    pub fn acknowledge(&mut self, id: String) {
        self.request(Request::Acknowledge { event: id });
    }
    /// Back to the job's own view.
    pub fn show_overview(&mut self) {
        self.view = View::Job;
    }
    /// Move the terminal frame's window a screen through the transcript,
    /// and hold it there — unless that is back to the end, where it takes
    /// up following the tool again. Says whether the frame is up to be
    /// scrolled at all, so the press can be spent elsewhere when it is not.
    ///
    /// A screen less a row, so the last row of one screen is the first of
    /// the next and nothing is read past.
    pub fn scroll(&mut self, forward: bool) -> bool {
        if self.view != View::Output {
            return false;
        }
        let count = terminal_rows(&self.output_lines()).len();
        let top = self.scroll.top(count, TERMINAL_ROWS);
        let step = TERMINAL_ROWS.saturating_sub(1).max(1);
        let last = count.saturating_sub(TERMINAL_ROWS);
        let to = if forward {
            top.saturating_add(step).min(last)
        } else {
            top.saturating_sub(step)
        };
        self.scroll = if to >= last {
            Scroll::Following
        } else {
            Scroll::At(to)
        };
        true
    }
    /// The terminal frame: the window of the transcript that is on show, and
    /// where in the whole it is.
    fn terminal(&self) -> Line {
        let rows = terminal_rows(&self.output_lines());
        let top = self.scroll.top(rows.len(), TERMINAL_ROWS);
        let shown: Vec<String> = rows.iter().skip(top).take(TERMINAL_ROWS).cloned().collect();
        let foot = if rows.is_empty() {
            crate::i18n::text("shell-nothing-said-yet").to_owned()
        } else if rows.len() <= TERMINAL_ROWS {
            crate::message!("count-lines", "count" => rows.len())
        } else if self.scroll == Scroll::Following {
            crate::message!("terminal-lines-following", "from" => top + 1, "to" => rows.len(), "total" => rows.len())
        } else {
            crate::message!("terminal-lines-scroll", "from" => top + 1, "to" => (top + TERMINAL_ROWS).min(rows.len()), "total" => rows.len())
        };
        Line::Terminal {
            lines: shown,
            rows: TERMINAL_ROWS,
            foot,
        }
    }
    /// The sources this job is about, in the coordinator's order.
    fn selected(&self) -> impl Iterator<Item = &Source> {
        self.snapshot
            .sources
            .iter()
            .filter(|s| self.snapshot.selected.contains(&s.id))
    }
    /// The last thing the tools said, which is where a question is.
    fn last_line(&self) -> Option<&str> {
        self.snapshot
            .output
            .iter()
            .rev()
            .map(|s| s.trim_end())
            .find(|s| !s.is_empty())
    }
    /// Whether the active tool is waiting on a yes or a no right now — a
    /// question at the end of its output that has not been answered from
    /// here, and not a password, which is a field rather than a button.
    fn asking_yes_or_no(&self) -> bool {
        self.snapshot.phase == Phase::Running
            && !self.snapshot.secret
            && self.last_line().is_some_and(|line| {
                asks_yes_or_no(line)
                    && !self.answered.as_ref().is_some_and(|(at, answered)| {
                        *at == lxb_updates::prompt::position(&self.snapshot)
                            && line.starts_with(answered)
                    })
            })
    }
    /// How far the job is: things passed over things to do, across every
    /// source it is about.
    ///
    /// A source the job is past counts all of its items, whatever became of
    /// them — the bar is where the job is, not how well it went, which is
    /// the finish's to say; the active one counts what its tool has said it
    /// is on — `(2/4) upgrading mesa`, `Updating 1/1…` — and the rest count
    /// nothing yet. Nothing, rather than a share of the sources, when no
    /// source listed anything to count: a bar that stands at nought for a
    /// minute and then jumps to the end is worse than lights.
    fn progress(&self) -> Option<(usize, usize)> {
        let mut done = 0;
        let mut total = 0;
        for source in self.selected() {
            let count = if source.listed && source.error.is_none() {
                source.items.len()
            } else {
                0
            };
            if let Some(result) = self.snapshot.results.iter().find(|r| r.source == source.id) {
                if !result.success {
                    return None;
                }
                done += count;
                total += count;
            } else if self.snapshot.active == Some(source.id) {
                match counted_output(&self.snapshot.output, source.id, count) {
                    Some((at, of)) => {
                        done += at.saturating_sub(1);
                        total += of.max(count);
                    }
                    None => total += count,
                }
            } else {
                total += count;
            }
        }
        (total > 0).then_some((done.min(total), total))
    }
    fn send_input(&mut self, text: Secret) {
        if self
            .send
            .send(Work::Input {
                job: self.snapshot.job,
                text,
            })
            .is_ok()
        {
            self.pending += 1;
            self.awaiting = true;
        }
    }
    pub fn submit(&mut self) {
        if self.typing {
            let text = std::mem::take(&mut self.typed);
            self.send_input(text);
            self.typing = false;
        }
    }
    pub fn type_into(&mut self, stroke: Stroke) -> bool {
        if !self.open || !self.typing {
            return false;
        }
        match stroke {
            Stroke::ENTER => self.submit(),
            Stroke::ESCAPE => {
                self.typing = false;
                self.typed = Secret::default();
            }
            Stroke::BACKSPACE => self.typed.pop(),
            Stroke::Char(c) if !c.is_control() => self.typed.push(c),
            _ => {}
        }
        true
    }
    pub fn poll(&mut self, in_settings: bool) -> (bool, bool) {
        let mut changed = false;
        let mut rows_changed = false;
        while let Ok(response) = self.receive.try_recv() {
            self.pending = self.pending.saturating_sub(1);
            if self.pending == 0 {
                self.awaiting = false;
            }
            match response {
                Ok(response) => {
                    self.pending_events = response.events.iter().map(|e| e.id.clone()).collect();
                    self.seen_events
                        .retain(|id| self.pending_events.contains(id));
                    for event in response.events {
                        if self.seen_events.insert(event.id.clone()) {
                            self.events.push(event);
                        }
                    }
                    if let Some(chunk) = response.log {
                        if self.output_job == Some(chunk.job) && self.log_next == chunk.offset {
                            self.log.push_bytes(&chunk.bytes);
                            self.log_next = chunk.next;
                            self.log_total = chunk.total;
                            // Everything the coordinator has, and it is not
                            // writing any more. Judged against the snapshot
                            // as it stood when the chunk was asked for; a
                            // job that starts running again clears it.
                            self.log_settled = chunk.next >= chunk.total
                                && !(chunk.job == self.snapshot.job && self.snapshot.busy());
                            changed = true;
                        }
                    }
                    if let Some(error) = response.error {
                        if self.view == View::Output {
                            self.log_error = Some(error.clone());
                        }
                        self.error = Some(error);
                    }
                    self.daily = response.daily_check;
                    if response.history_requested {
                        self.history = response.history;
                    }
                    // A job that has just stopped: what it wrote last is
                    // still to be fetched, and Recent updates has a new row
                    // to show. Both are asked for below rather than here,
                    // so one turn of the loop makes one request.
                    let ended = self.snapshot.busy() && !response.snapshot.busy();
                    changed |= self.snapshot != response.snapshot || self.open;
                    // Echo-off also occurs during Flatpak's progress display.
                    // Require an actual prompt before asking for a password.
                    // Re-evaluate output too: the text can arrive after echo
                    // changed, or before it. Escape still leaves the field.
                    let password = lxb_updates::prompt::password(&response.snapshot)
                        && !lxb_updates::prompt::password(&self.snapshot);
                    self.snapshot = response.snapshot;
                    if password && self.open && self.view == View::Job {
                        self.edit();
                    }
                    // The column's rows follow the sources — but not
                    // through a check, which rebuilds them one source at a
                    // time: a row that said "4 updates" and then "Not
                    // checked yet" and then "4 updates" again, behind the
                    // panel that was checking, was the page changing under
                    // the person watching it. They change when the check
                    // has finished.
                    if !self.snapshot.sources.is_empty() && self.snapshot.phase != Phase::Checking {
                        let mut held = SOURCES
                            .get_or_init(|| Mutex::new(vec![]))
                            .lock()
                            .unwrap_or_else(|p| p.into_inner());
                        rows_changed = *held != self.snapshot.sources;
                        held.clone_from(&self.snapshot.sources);
                    }
                    if self.snapshot.phase != Phase::Running {
                        self.typing = false;
                        self.typed = Secret::default();
                        self.answered = None;
                    }
                    if ended {
                        self.log_settled = false;
                        self.history_stale = true;
                    }
                }
                Err(error) => {
                    self.error = Some(error);
                    changed = true;
                    // A coordinator that cannot be reached is not asked again
                    // every second: each attempt is a service start and a
                    // fallback spawn, several seconds of them, and a journal
                    // line for every one. A press still asks at once.
                    self.next = Instant::now() + Duration::from_secs(30);
                    self.next_check = Instant::now() + Duration::from_secs(300);
                }
            }
        }
        // Recent updates is a list of what has ended, so it only ever
        // changes when something ends — and then only matters if somebody
        // is looking at it. Ahead of the routine status poll: the view on
        // show is the one waiting on an answer, and a status the panel is
        // not drawing can wait a turn.
        if self.pending == 0 && self.open && self.view == View::History && self.history_stale {
            self.history_stale = false;
            self.request(Request::History);
        }
        if self.pending == 0
            && Instant::now() >= self.next
            && (in_settings || self.open || self.snapshot.busy())
            && !(self.open
                && self.view == View::Output
                && self.output_job == Some(self.snapshot.job)
                && self.snapshot.phase == Phase::Running)
        {
            self.request(Request::Status);
            self.next = Instant::now() + Duration::from_secs(1);
        }
        if self.pending == 0 && self.view == View::Output && self.open && self.log_error.is_none() {
            if let Some(job) = self.output_job {
                let writing = job == self.snapshot.job && self.snapshot.phase == Phase::Running;
                let due = Instant::now() >= self.next;
                // More is known to be waiting: fetch it now rather than on
                // the tick, so a long transcript arrives at the speed of the
                // round trip and not a chunk a second. Otherwise ask on the
                // tick while the tool is still writing, and once more after
                // it stops — see [`Self::log_settled`].
                if self.log_next < self.log_total || ((writing || !self.log_settled) && due) {
                    self.request(Request::Output {
                        job,
                        offset: self.log_next,
                    });
                    self.next = Instant::now() + Duration::from_secs(1);
                }
            }
        }
        if self.pending == 0 && Instant::now() >= self.next_events {
            self.request(Request::Events);
            self.next_events = Instant::now() + Duration::from_secs(3);
        }
        if self.pending == 0
            && in_settings
            && !self.open
            && !self.snapshot.busy()
            && self.daily
            && Instant::now() >= self.next_check
        {
            let last = self
                .snapshot
                .sources
                .iter()
                .filter_map(|s| s.checked)
                .min()
                .unwrap_or(0);
            if lxb_updates::now().saturating_sub(last) >= 86400 {
                self.request(Request::Check { selected: vec![] });
            }
            self.next_check = Instant::now() + Duration::from_secs(3600);
        }
        (rows_changed, changed)
    }

    pub fn panel(&self) -> (Vec<Line>, Vec<menu::Entry>) {
        use menu::{Command as C, Entry as E};
        let mut lines = vec![Line::Heading(
            match self.view {
                View::Job => crate::i18n::text("shell-updates"),
                View::Output => crate::i18n::text("shell-full-output"),
                View::History => crate::i18n::text("shell-recent-updates"),
                View::Preferences => crate::i18n::text("shell-update-preferences"),
            }
            .into(),
        )];
        let mut buttons = vec![];
        if self.awaiting {
            lines.push(Line::Note(crate::i18n::text("shell-one-moment").into()));
            lines.push(Line::Waiting);
            buttons.push(E::new(C::Dismiss, crate::i18n::text("shell-close")));
            return (lines, buttons);
        }
        if let Some(error) = &self.error {
            lines.push(Line::Note(
                crate::i18n::text("shell-could-not-complete-the-request").into(),
            ));
            match self.view {
                View::Job => lines.push(Line::Note(
                    crate::i18n::text("shell-open-details-for-the-reason").into(),
                )),
                View::Output => {}
                View::History | View::Preferences => {
                    lines.extend(wrap(error, WIDTH).into_iter().map(Line::Note));
                }
            }
        }
        // The way out, worded for what leaving does: a review is declined,
        // a job is left to carry on, a result is closed.
        let mut leaving = crate::i18n::text("shell-close");
        match self.view {
            View::Preferences => {
                lines.extend(
                    wrap(crate::i18n::text("updates-daily-checks-explanation"), WIDTH)
                        .into_iter()
                        .map(Line::Note),
                );
                lines.push(Line::field(
                    crate::i18n::text("shell-daily-checks"),
                    if self.daily {
                        crate::i18n::text("shell-on")
                    } else {
                        crate::i18n::text("shell-off")
                    },
                ));
                buttons.push(E::new(
                    C::UpdateDaily,
                    if self.daily {
                        crate::i18n::text("shell-turn-off-daily-checks")
                    } else {
                        crate::i18n::text("shell-turn-on-daily-checks")
                    },
                ));
            }
            View::History => {
                if self.history.is_empty() {
                    lines.push(Line::Note(
                        crate::i18n::text("shell-no-recent-updates").into(),
                    ));
                }
                for attempt in self.history.iter().rev().take(3) {
                    buttons.push(E::new(
                        C::UpdateLog(attempt.job),
                        format!(
                            "{} · {}",
                            history_date(attempt.started),
                            phase_name(&attempt.phase)
                        ),
                    ));
                }
            }
            View::Output => {
                lines.push(self.terminal());
                if self.scroll != Scroll::Following {
                    buttons.push(E::new(
                        C::UpdateLive,
                        crate::i18n::text("shell-latest-output"),
                    ));
                }
                if self.snapshot.phase == Phase::Running
                    && self.output_job == Some(self.snapshot.job)
                {
                    buttons.push(E::new(C::UpdateRespond, crate::i18n::text("shell-respond")));
                }
                buttons.push(E::new(C::UpdateOutputBack, crate::i18n::text("shell-back")));
                if self.snapshot.busy() {
                    leaving = crate::i18n::text("shell-run-in-background");
                }
            }
            View::Job => match self.snapshot.phase {
                Phase::Checking => {
                    lines.push(Line::Note(
                        crate::i18n::text("shell-checking-for-updates").into(),
                    ));
                    lines.push(Line::Waiting);
                    buttons.push(E::new(
                        C::UpdateCancelCheck,
                        crate::i18n::text("shell-cancel"),
                    ));
                    leaving = crate::i18n::text("shell-run-in-background");
                }
                Phase::Running => {
                    self.running(&mut lines, &mut buttons);
                    leaving = crate::i18n::text("shell-run-in-background");
                }
                Phase::Restarting => {
                    lines.push(Line::Note(
                        crate::i18n::text("shell-restarting-ellipsis").into(),
                    ));
                    lines.push(Line::Waiting);
                }
                Phase::Reviewing => {
                    if self.review(&mut lines, &mut buttons) {
                        leaving = crate::i18n::text("shell-not-now");
                    }
                }
                _ => self.outcome(&mut lines, &mut buttons),
            },
        }
        buttons.push(E::new(C::Dismiss, leaving));
        (lines, buttons)
    }

    /// The review: what the check found, as a count and one line a source,
    /// and the one button that installs it. Says whether that button is
    /// there.
    fn review(&self, lines: &mut Vec<Line>, buttons: &mut Vec<menu::Entry>) -> bool {
        use menu::{Command as C, Entry as E};
        let tally = Tally::of(self.selected());
        lines.push(Line::Note(tally.headline()));
        for source in self.selected() {
            lines.push(Line::field(
                crate::i18n::builtin(source.id.name()),
                summary(source),
            ));
        }
        // What stands in the way, before the button that would ignore it.
        lines.extend(warnings(&self.snapshot.notices));
        // A restart already staged by the last job is finished first; a
        // second system deployment behind an unfinished one is what the
        // coordinator refuses, and the button for it is the one offered.
        let staged = self.snapshot.restart.is_some();
        let installable = tally.something_to_do()
            && !(staged && self.snapshot.selected.contains(&SourceId::System));
        if installable {
            if tally.stale {
                lines.push(Line::Note(
                    crate::i18n::text("shell-the-package-manager-refreshes-its-list-when-it-runs")
                        .into(),
                ));
            }
            if self.restart_expected() {
                lines.push(Line::Note(
                    crate::i18n::text("shell-a-restart-may-be-needed-afterwards").into(),
                ));
            }
            buttons.push(E::new(
                C::UpdateInstall,
                crate::i18n::text("shell-update-now"),
            ));
        }
        if staged && !self.snapshot.busy() {
            buttons.push(E::new(
                C::UpdateRestart,
                crate::i18n::text("shell-restart-to-finish-updates"),
            ));
        }
        buttons.push(E::new(
            C::UpdateOutput,
            crate::i18n::text("shell-full-output"),
        ));
        installable
    }

    /// Whether what is about to be installed is the kind that wants a
    /// restart: the system's own packages, or a device's firmware.
    fn restart_expected(&self) -> bool {
        self.selected().any(|s| {
            s.executable
                && s.error.is_none()
                && match s.id {
                    SourceId::System => !s.listed || !s.items.is_empty() || !s.fresh,
                    SourceId::Firmware => !s.items.is_empty(),
                    _ => false,
                }
        })
    }

    /// The job under way: which source it is on, how far it is, the last
    /// thing its tool said, and the answer to that if it was a question.
    fn running(&self, lines: &mut Vec<Line>, buttons: &mut Vec<menu::Entry>) {
        use menu::{Command as C, Entry as E};
        if self.snapshot.active.is_none() && self.snapshot.output.is_empty() {
            lines.push(Line::Note(
                crate::i18n::text("shell-preparing-your-updates").into(),
            ));
            lines.push(Line::Waiting);
            lines.push(Line::Note(
                crate::i18n::text("shell-you-may-be-asked-to-authorize-this-update").into(),
            ));
            buttons.push(E::new(
                C::UpdateOutput,
                crate::i18n::text("shell-full-output"),
            ));
            return;
        }
        // Which source this output belongs to, and how far along the list
        // it is — a prompt on its own does not say whether it is flatpak's
        // or fwupd's, and a person who hid the panel and came back has no
        // other way to know.
        if let Some(active) = self.snapshot.active {
            let selected = &self.snapshot.selected;
            let at = selected.iter().position(|id| *id == active);
            lines.push(Line::field(
                crate::i18n::text("shell-updating"),
                match at {
                    Some(at) if selected.len() > 1 => {
                        crate::message!("updates-source-progress", "source" => crate::i18n::builtin(active.name()), "at" => at + 1, "of" => selected.len())
                    }
                    _ => crate::i18n::builtin(active.name()).to_owned(),
                },
            ));
        }
        let output: Vec<String> = self
            .snapshot
            .output
            .iter()
            .flat_map(|s| wrap(s, WIDTH))
            .collect();
        let said = |lines: &mut Vec<Line>, count: usize| {
            lines.extend(
                output
                    .iter()
                    .skip(output.len().saturating_sub(count))
                    .cloned()
                    .map(Line::Note),
            );
        };
        if self.typing {
            // The question and the field, and no bar: half the display is
            // keyboard now, and what has to fit above it is the thing being
            // answered.
            said(lines, 2);
            lines.push(Line::Note(
                if lxb_updates::prompt::password(&self.snapshot) {
                    crate::i18n::text("shell-type-the-password-it-asks-for")
                } else {
                    crate::i18n::text("shell-type-the-answer-it-asks-for")
                }
                .into(),
            ));
            lines.push(Line::Secret {
                typed: self.typed.typed(),
            });
            buttons.push(E::new(C::UpdateSubmit, crate::i18n::text("shell-send")));
            return;
        }
        // The bar where there is something to count, and the lights where
        // there is not; the two are the same height, so the panel does not
        // move under a thumb as one source ends and the next begins.
        match self.progress() {
            Some((done, total)) => {
                let percent = (done * 100 / total) as u8;
                lines.push(Line::Note(crate::message!("updates-progress", "done" => done, "total" => total, "percent" => percent)));
                lines.push(Line::Progress(percent));
            }
            None => {
                lines.push(Line::Note(crate::i18n::text("shell-working").into()));
                lines.push(Line::Waiting);
            }
        }
        let question = self.asking_yes_or_no()
            || lxb_updates::prompt::password(&self.snapshot)
            || self
                .last_line()
                .is_some_and(|line| line.ends_with(':') || line.ends_with('?'));
        if question {
            said(lines, 2);
        } else if self.snapshot.protected {
            lines.push(Line::Note(
                crate::i18n::text("shell-sleep-and-shutdown-are-paused-while-updating").into(),
            ));
        }
        // A question with two answers gets the two answers; anything else a
        // tool asks — a number to pick a provider by, an answer this panel
        // does not recognise as a question — gets the field.
        if self.asking_yes_or_no() {
            buttons.push(E::new(C::UpdateYes, crate::i18n::text("shell-yes")));
            buttons.push(E::new(C::UpdateNo, crate::i18n::text("shell-no")));
        } else if question {
            buttons.push(E::new(C::UpdateRespond, crate::i18n::text("shell-respond")));
        }
        buttons.push(E::new(
            C::UpdateOutput,
            crate::i18n::text("shell-full-output"),
        ));
    }

    /// What became of the job: a sentence, a word a source, and the restart
    /// if one is owed.
    fn outcome(&self, lines: &mut Vec<Line>, buttons: &mut Vec<menu::Entry>) {
        use menu::{Command as C, Entry as E};
        lines.extend(
            wrap(phase_note(&self.snapshot.phase), WIDTH)
                .into_iter()
                .map(Line::Note),
        );
        for result in &self.snapshot.results {
            lines.push(Line::field(
                crate::i18n::builtin(result.source.name()),
                outcome_word(result),
            ));
        }
        // Anything that happened to the job but not to a package: a log that
        // could not be written, authority that could not be given back. The
        // phase above is what the providers did and says nothing about
        // these, so if they were not here they would be nowhere.
        lines.extend(warnings(&self.snapshot.notices));
        let staged = self.snapshot.restart.is_some() && !self.snapshot.busy();
        if staged {
            lines.push(Line::Note(
                crate::i18n::text("shell-restart-to-finish-installing-them").into(),
            ));
            buttons.push(E::new(
                C::UpdateRestart,
                crate::i18n::text("shell-restart-now"),
            ));
        } else if self.snapshot.phase == Phase::Completed
            && self
                .snapshot
                .results
                .iter()
                .any(|r| r.success && r.source == SourceId::System)
        {
            lines.push(Line::Note(
                crate::i18n::text("shell-a-restart-may-be-needed").into(),
            ));
        }
        buttons.push(E::new(
            C::UpdateOutput,
            crate::i18n::text("shell-full-output"),
        ));
        if self.snapshot.phase != Phase::Idle {
            buttons.push(E::new(
                C::UpdateCheck,
                crate::i18n::text("shell-check-again"),
            ));
        }
    }
}

/// The half of a notice that goes on the panel.
///
/// The coordinator writes a notice as `headline · detail` — "Storage is
/// nearly full · /boot has 173 MiB free; the native manager checks whether
/// the transaction fits". The headline is the plain sentence somebody about
/// to press Update needs; the mount path and the megabytes are technical and
/// belong in Full output, where the whole line is written. A notice with no
/// `·` is short enough to be both halves.
fn headline(notice: &str) -> &str {
    notice.split(" · ").next().unwrap_or(notice).trim()
}

/// The conditions worth putting in front of somebody, headline only.
///
/// Two at most. A panel is one thing to read and a decision to make, and a
/// machine with six warnings on it has turned back into the report this page
/// was rebuilt to stop being; the rest are in Full output with their detail.
fn warnings(notices: &[String]) -> Vec<Line> {
    notices
        .iter()
        .take(2)
        .flat_map(|notice| wrap(headline(notice), WIDTH))
        .map(Line::Note)
        .collect()
}

/// The transcript as rows of the terminal frame: every line the tools said,
/// and a line longer than the terminal is wide folded onto the rows it
/// would have taken there.
///
/// Folded at [`COLUMNS`] characters, which is where the PTY the tools wrote
/// to folded it — a tool that measured its line to the terminal never
/// reaches this, and one that did not (a long download address, a warning
/// with a path in it) is shown whole rather than cut with an ellipsis,
/// exactly as the terminal would have shown it. Counted in characters
/// rather than columns; a transcript in a double-width script is folded a
/// little early, which is the cheaper of the two mistakes.
fn terminal_rows(output: &[String]) -> Vec<String> {
    let mut rows = Vec::with_capacity(output.len());
    for line in output {
        let line = line.trim_end();
        if line.chars().count() <= COLUMNS {
            rows.push(line.to_owned());
            continue;
        }
        let chars: Vec<char> = line.chars().collect();
        rows.extend(chars.chunks(COLUMNS).map(|c| c.iter().collect::<String>()));
    }
    rows
}

/// What a set of sources adds up to: the headline of a review, and the note
/// under "Update everything".
struct Tally {
    /// Things to install, over the sources that listed theirs.
    counted: usize,
    /// A source that can be run but does not list what it would change —
    /// an rpm-ostree deployment, a NixOS configuration.
    ready_uncounted: bool,
    /// A source that listed nothing from a package database it could not
    /// refresh without root, and refreshes when it runs: an APT machine's
    /// "up to date" is only as good as its last `apt-get update`.
    stale: bool,
    /// Nothing has been checked at all.
    unchecked: bool,
}

impl Tally {
    fn of<'a>(sources: impl Iterator<Item = &'a Source>) -> Self {
        let mut tally = Self {
            counted: 0,
            ready_uncounted: false,
            stale: false,
            unchecked: true,
        };
        for source in sources {
            if source.checked.is_some() || source.error.is_some() {
                tally.unchecked = false;
            }
            if !source.executable || source.error.is_some() || source.checked.is_none() {
                continue;
            }
            if !source.listed {
                tally.ready_uncounted = true;
            } else if source.items.is_empty() {
                tally.stale |= !source.fresh;
            } else {
                tally.counted += source.items.len();
            }
        }
        tally
    }
    fn something_to_do(&self) -> bool {
        self.counted > 0 || self.ready_uncounted || self.stale
    }
    fn headline(&self) -> String {
        match self.counted {
            _ if self.unchecked => crate::i18n::text("shell-not-checked-yet").into(),
            0 if self.ready_uncounted => crate::i18n::text("shell-ready-to-update").into(),
            0 if self.stale => crate::i18n::text("shell-nothing-new-since-the-last-refresh").into(),
            0 => crate::i18n::text("shell-everything-is-up-to-date").into(),
            n => crate::message!("count-updates-available", "count" => n),
        }
    }
}

/// How many characters a line of the panel holds before it is wrapped here
/// rather than clipped there — see [`crate::dialog`], which draws a note on
/// one line whatever its length.
const WIDTH: usize = 46;

/// The state of a source in a few words, for the value of a field and the
/// note under a row: what a check made of it, or why it could not be run.
///
/// Short on purpose, so it fits beside its name. The reason behind "Could
/// not check" is in Details; the number is a number because the coordinator
/// counts things, not lines — see [`lxb_updates::listing`].
fn summary(source: &Source) -> String {
    let mut line = if source.error.is_some() {
        crate::i18n::text("shell-could-not-check").to_owned()
    } else if !source.executable {
        first_segment(&source.note).to_owned()
    } else if source.checked.is_none() {
        crate::i18n::text("shell-not-checked-yet").to_owned()
    } else if !source.listed {
        crate::i18n::text("shell-ready-to-update").to_owned()
    } else {
        match (source.id, source.items.len()) {
            // Not "up to date": the devices fwupd may not touch from here
            // are the excluded ones beside it, and they may well have some.
            (SourceId::Firmware, 0) => crate::i18n::text("shell-no-device-updates").to_owned(),
            (_, 0) if !source.fresh => {
                crate::i18n::text("shell-none-since-the-last-refresh").to_owned()
            }
            (_, 0) => crate::i18n::text("shell-up-to-date").to_owned(),
            (SourceId::Firmware, n) => crate::message!("count-device-updates", "count" => n),
            (_, n) => crate::message!("count-updates", "count" => n),
        }
    };
    if !source.excluded.is_empty() {
        line.push_str(" · ");
        line.push_str(&crate::message!("count-excluded", "count" => source.excluded.len()));
    }
    line
}

/// What became of a source, in a word: "Finished", "Staged for the next
/// boot", "Deferred" — or "Failed", with the tool's reason kept for Details
/// rather than cut to fit a field.
fn outcome_word(result: &lxb_updates::ResultEntry) -> String {
    if result.success || result.note.starts_with("Deferred") || result.note.starts_with("Skipped") {
        first_segment(&result.note).to_owned()
    } else {
        crate::i18n::text("shell-failed").to_owned()
    }
}

/// A note's headline: the part before its first " · ", which is how the
/// coordinator writes them — a state, then the explanation.
fn first_segment(note: &str) -> &str {
    note.split(" · ")
        .next()
        .unwrap_or(note)
        .trim_end_matches('.')
}

/// Whether a line of a tool's output is it asking for a yes or a no —
/// pacman's `[Y/n]`, DNF's `[y/N]:`, zypper's `[y/n/v/...? shows all
/// options] (y):`, emerge's `[Yes/No]`, fwupd's `[Y|n]:`.
fn asks_yes_or_no(line: &str) -> bool {
    lxb_updates::prompt::yes_or_no(line)
}

/// Where the active source's tool is in its list, from what it has said
/// since the coordinator's heading for that source.
///
/// The line that counts `at/of` and names something being installed, and of
/// those the one furthest along — not the last, because a tool counts more
/// than its packages: pacman's hooks after them are `(1/12) Updating icon
/// theme caches…`, and a bar that followed the last count would run to the
/// end and start again. The furthest count never goes backwards, and what
/// comes after the packages is at most the rest of the source's share.
/// Lines that count something else without naming an installation — keys
/// being checked `(4/4)`, DNF verifying — are not the list and are left
/// out. APT counts nothing, and is measured by the packages it has set up.
fn counted_output(output: &[String], active: SourceId, items: usize) -> Option<(usize, usize)> {
    let heading = format!("— {} —", active.name());
    let start = output.iter().rposition(|line| line == &heading)?;
    let said = &output[start + 1..];
    let counted = said
        .iter()
        .filter(|line| {
            let line = line.to_ascii_lowercase();
            [
                "upgrad",
                "install",
                "updat",
                "remov",
                "downgrad",
                "unpack",
                "setting up",
                "configur",
            ]
            .iter()
            .any(|verb| line.contains(verb))
        })
        .filter_map(|line| fraction(line))
        .fold(None, |best: Option<(usize, usize)>, (at, of)| match best {
            Some((best_at, best_of)) if at * best_of < best_at * of => best,
            _ => Some((at, of)),
        });
    if counted.is_some() {
        return counted;
    }
    let set_up = said
        .iter()
        .filter(|line| line.starts_with("Setting up "))
        .count();
    (set_up > 0 && items > 0).then(|| (set_up.min(items), items))
}

/// The first `digits/digits` in a line, as numbers.
fn fraction(line: &str) -> Option<(usize, usize)> {
    for (at, _) in line.match_indices('/') {
        let before: String = line[..at]
            .chars()
            .rev()
            .take_while(char::is_ascii_digit)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let after: String = line[at + 1..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        if let (Ok(a), Ok(b)) = (before.parse::<usize>(), after.parse::<usize>()) {
            if b > 0 && a <= b {
                return Some((a, b));
            }
        }
    }
    None
}

fn history_date(at: u64) -> String {
    let time = at as libc::time_t;
    let mut date = std::mem::MaybeUninit::<libc::tm>::uninit();
    if unsafe { libc::localtime_r(&time, date.as_mut_ptr()) }.is_null() {
        return at.to_string();
    }
    let date = unsafe { date.assume_init() };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        date.tm_year + 1900,
        date.tm_mon + 1,
        date.tm_mday,
        date.tm_hour,
        date.tm_min
    )
}

fn phase_name(phase: &Phase) -> &'static str {
    match phase {
        Phase::Idle => crate::i18n::text("shell-not-checked"),
        Phase::Checking => crate::i18n::text("shell-checking"),
        Phase::Reviewing => crate::i18n::text("shell-reviewed"),
        Phase::Running => crate::i18n::text("shell-running"),
        Phase::Completed => crate::i18n::text("shell-completed"),
        Phase::Partial => crate::i18n::text("shell-needs-attention"),
        Phase::Failed => crate::i18n::text("shell-failed"),
        Phase::Interrupted => crate::i18n::text("shell-interrupted"),
        Phase::Cancelled => crate::i18n::text("shell-cancelled"),
        Phase::Restarting => crate::i18n::text("shell-restarting"),
        Phase::AwaitingVerification => crate::i18n::text("shell-restarted-unverified"),
    }
}

fn phase_note(phase: &Phase) -> &'static str {
    match phase {
        Phase::Idle => crate::i18n::text("shell-not-checked-yet-full-stop"),
        Phase::Checking => crate::i18n::text("shell-checking-for-updates"),
        Phase::Reviewing => crate::i18n::text("shell-ready-full-stop"),
        Phase::Running => crate::i18n::text("shell-updating-ellipsis"),
        Phase::Completed => crate::i18n::text("shell-updates-installed-full-stop"),
        Phase::Partial => crate::i18n::text("shell-some-updates-need-attention"),
        Phase::Failed => crate::i18n::text("shell-the-updates-could-not-be-installed"),
        Phase::Interrupted => {
            crate::i18n::text("shell-the-last-update-was-interrupted-before-it-finished")
        }
        Phase::Cancelled => crate::i18n::text("shell-check-cancelled-nothing-was-installed"),
        Phase::Restarting => crate::i18n::text("shell-restarting-ellipsis"),
        Phase::AwaitingVerification => {
            crate::i18n::text("shell-the-machine-restarted-check-again-to-confirm")
        }
    }
}

fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = vec![];
    for paragraph in text.lines() {
        let mut line = String::new();
        for word in paragraph.split_whitespace() {
            if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width {
                lines.push(std::mem::take(&mut line));
            }
            for part in word.chars().collect::<Vec<_>>().chunks(width) {
                if !line.is_empty() {
                    line.push(' ');
                }
                line.extend(part);
                if line.chars().count() >= width {
                    lines.push(std::mem::take(&mut line));
                }
            }
        }
        if !line.is_empty() {
            lines.push(line);
        }
    }
    lines
}

impl crate::Shell {
    pub(crate) fn sync_updates(&mut self) {
        let in_settings = self.standing_in_settings();
        let (rows, panel) = self.updates.poll(in_settings);
        for event in std::mem::take(&mut self.updates.events) {
            if event.attention
                && (self.updates.snapshot.job != event.job
                    || self.updates.snapshot.phase != Phase::Running
                    || !self.updates.pending_events.contains(&event.id))
            {
                self.updates.acknowledge(event.id);
                continue;
            }
            let title = if event.attention {
                crate::i18n::text("shell-updates-need-your-attention")
            } else if event.restart.is_some()
                && !matches!(event.phase, Phase::Failed | Phase::Interrupted)
            {
                crate::i18n::text("shell-restart-to-finish-updating")
            } else if event.phase == Phase::Completed {
                crate::i18n::text("shell-updates-installed")
            } else {
                crate::i18n::text("shell-some-updates-couldn-t-be-installed")
            };
            let action = if event.attention {
                crate::i18n::text("shell-review-prompt")
            } else if event.restart.is_some() {
                crate::i18n::text("shell-review-restart")
            } else {
                crate::i18n::text("shell-view-output")
            };
            let visible = self.updates.shows_job(event.job);
            let (id, raised) = self.notifications.announce_update(
                title,
                crate::i18n::text("shell-open-updates-to-see-the-result"),
                action,
                visible || event.delivered,
            );
            if raised {
                self.sounds.notified();
            }
            self.updates.request(Request::Delivered {
                event: event.id.clone(),
            });
            self.updates.notifications.insert(id, event);
            self.needs_redraw = true;
        }
        // A replied-to prompt (or a job that finished) must not leave an
        // obsolete “Review prompt” action in the notification centre.
        let stale = self.updates.stale_attention_notifications();
        for id in &stale {
            self.notifications.dismiss(*id);
        }
        self.needs_redraw |= !stale.is_empty();
        let dismissed: Vec<_> = self
            .updates
            .notifications
            .keys()
            .copied()
            .filter(|id| !self.notifications.list().iter().any(|n| n.id == *id))
            .collect();
        for id in dismissed {
            if let Some(event) = self.updates.notifications.remove(&id) {
                self.updates.acknowledge(event.id);
            }
        }
        if rows {
            let selected: Vec<_> = self
                .panels
                .iter()
                .map(|p| {
                    p.cursor
                        .current_entry(&self.lattice)
                        .and_then(crate::apps::Entry::setting)
                })
                .collect();
            self.rebuild_settings();
            for (panel, selected) in self.panels.iter_mut().zip(selected) {
                if let Some(crate::settings::Setting::Update(_)) = selected {
                    let at = panel
                        .cursor
                        .current_entries(&self.lattice)
                        .iter()
                        .position(|e| e.setting() == selected);
                    if let Some(at) = at {
                        panel.cursor.point_at_row(at, &self.lattice);
                    }
                }
            }
            self.needs_redraw = true;
        }
        // The panel set aside for a question `polkitd` asked — see
        // [`Updates::set_aside`] — comes back the moment the question's own
        // panel has gone, onto the screen it was taken off. Unless something
        // else has the screen by then: a panel of its own, the guide, a
        // menu. Then it is not brought up over that, and the job carries on
        // in the background exactly as if the person had said so — the
        // page still says what it is doing, and a press on the row brings
        // it back.
        if self.updates.aside && self.authenticating.is_none() {
            if self.dialog.is_open() || self.guide.is_menu() || self.context_menu.is_on_screen() {
                self.updates.aside = false;
                self.updates.hide();
            } else if !self.dialog.is_on_screen() {
                self.updates.aside = false;
                self.show_updates();
                return;
            }
            // Otherwise the question's panel is still folding away, and the
            // frame after this one is the one.
        }
        // A polkit question takes priority. Do not overwrite another panel
        // simply because a background job produced a line of output.
        let own_panel = self.dialog.is_open()
            && !self.updates.buttons.is_empty()
            && self
                .dialog
                .buttons
                .entries()
                .iter()
                .map(|e| e.command)
                .collect::<Vec<_>>()
                == self.updates.buttons;
        if panel && self.updates.open && !self.updates.aside && own_panel {
            self.show_updates();
        }
    }
    pub(crate) fn show_updates(&mut self) {
        let (lines, buttons) = self.updates.panel();
        let commands: Vec<_> = buttons.iter().map(|e| e.command).collect();
        if self.dialog.is_open() && self.updates.buttons == commands {
            self.dialog.say(lines);
        } else {
            let from = self.dialog_origin();
            self.dialog.ask(
                from,
                (!self.updates.typing).then(|| crate::icons::SETTING_UPDATES.into()),
                lines,
                buttons,
                0,
            );
            self.updates.buttons = commands;
        }
        self.osk.offer_shell_field(self.updates.typing);
        self.sync_surface_state();
        self.needs_redraw = true;
    }
    pub(crate) fn type_into_updates(&mut self, stroke: Stroke) -> bool {
        if !self.dialog.is_open()
            || self
                .dialog
                .buttons
                .entries()
                .iter()
                .map(|e| e.command)
                .collect::<Vec<_>>()
                != self.updates.buttons
        {
            return false;
        }
        if !self.updates.type_into(stroke) {
            return false;
        }
        self.show_updates();
        true
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn history_and_preferences_do_not_silence_background_completion() {
        let (send, _work) = mpsc::channel();
        let (_done, receive) = mpsc::channel();
        let mut updates = Updates::over(send, receive);
        updates.snapshot.job = 9;
        updates.open = true;
        assert!(updates.shows_job(9));
        updates.view = View::History;
        assert!(!updates.shows_job(9));
        updates.view = View::Preferences;
        assert!(!updates.shows_job(9));
        updates.view = View::Output;
        updates.output_job = Some(8);
        assert!(!updates.shows_job(9));
        updates.output_job = Some(9);
        assert!(updates.shows_job(9));
        updates.aside = true;
        assert!(!updates.shows_job(9));
    }

    #[test]
    fn answered_or_finished_prompts_leave_no_stale_notification_action() {
        let (send, _work) = mpsc::channel();
        let (_done, receive) = mpsc::channel();
        let mut updates = Updates::over(send, receive);
        updates.snapshot.job = 9;
        updates.snapshot.phase = Phase::Running;
        let event = lxb_updates::JobEvent {
            id: "9:attention:12".into(),
            job: 9,
            phase: Phase::Running,
            restart: None,
            attention: true,
            delivered: true,
        };
        updates.pending_events.insert(event.id.clone());
        updates.notifications.insert(7, event);
        assert!(updates.stale_attention_notifications().is_empty());
        updates.pending_events.clear();
        assert_eq!(updates.stale_attention_notifications(), [7]);
        updates.pending_events.insert("9:attention:12".into());
        updates.snapshot.phase = Phase::Completed;
        assert_eq!(updates.stale_attention_notifications(), [7]);
        updates.notifications.get_mut(&7).unwrap().attention = false;
        assert!(updates.stale_attention_notifications().is_empty());
    }

    #[test]
    fn routine_update_progress_keeps_native_output_in_full_output() {
        use menu::Command as C;
        let (send, _work) = mpsc::channel();
        let (_done, receive) = mpsc::channel();
        let mut updates = Updates::over(send, receive);
        updates.open = true;
        updates.snapshot.phase = Phase::Running;
        updates.snapshot.protected = true;
        updates.snapshot.authorized = true;
        updates.snapshot.active = Some(SourceId::Flatpak);
        updates.snapshot.output = vec!["Routine native diagnostics".into()];
        let (lines, _) = updates.panel();
        assert!(!notes(&lines)
            .iter()
            .any(|s| s.contains("Routine native diagnostics")));
        assert!(notes(&lines)
            .iter()
            .any(|s| s.contains("Sleep and shutdown")));
        // One way in to anything technical, and it is Full output.
        assert_eq!(commands(&updates), [C::UpdateOutput, C::Dismiss]);
        updates.show_output();
        assert!(commands(&updates).contains(&C::UpdateRespond));
        let (lines, _) = updates.panel();
        assert!(lines.iter().any(|line| matches!(line, Line::Terminal { lines: rows, .. } if rows.iter().any(|s| s.contains("Routine native diagnostics")))));
    }

    use super::*;

    fn checked(id: SourceId, items: usize, excluded: usize) -> Source {
        Source {
            id,
            provider: lxb_updates::Provider::Firmware,
            note: "Check complete · review provider details".into(),
            items: (0..items)
                .map(|i| lxb_updates::Item {
                    name: format!("thing-{i}"),
                    detail: String::new(),
                })
                .collect(),
            excluded: (0..excluded)
                .map(|i| lxb_updates::Item {
                    name: format!("kept-{i}"),
                    detail: "manual".into(),
                })
                .collect(),
            error: None,
            checked: Some(1),
            fresh: true,
            listed: true,
            executable: true,
            policy: None,
        }
    }

    fn reviewing(sources: Vec<Source>) -> Updates {
        let (send, _work) = mpsc::channel();
        let (_done, receive) = mpsc::channel();
        let mut updates = Updates::over(send, receive);
        updates.snapshot.phase = Phase::Reviewing;
        updates.snapshot.selected = sources.iter().map(|s| s.id).collect();
        updates.snapshot.sources = sources;
        updates
    }

    fn notes(lines: &[Line]) -> Vec<String> {
        lines
            .iter()
            .filter_map(|l| match l {
                Line::Note(n) => Some(n.clone()),
                _ => None,
            })
            .collect()
    }

    fn fields(lines: &[Line]) -> Vec<(String, String)> {
        lines
            .iter()
            .filter_map(|l| match l {
                Line::Field { label, value } => Some((label.clone(), value.clone())),
                _ => None,
            })
            .collect()
    }

    fn commands(updates: &Updates) -> Vec<menu::Command> {
        updates.panel().1.iter().map(|e| e.command).collect()
    }

    /// A review is a count, a word a source, and Update first. Photographed
    /// the other way on 2026-09-15: "Review before continuing. Native tools
    /// may still ask questions.", a line of prose a source, and "Continue
    /// with these sources" as the verb over three more buttons.
    #[test]
    fn a_review_is_a_count_and_one_button() {
        use menu::Command as C;
        let updates = reviewing(vec![
            checked(SourceId::System, 4, 0),
            checked(SourceId::Flatpak, 2, 0),
            checked(SourceId::Aur, 1, 0),
            checked(SourceId::Firmware, 1, 2),
        ]);
        let (lines, buttons) = updates.panel();
        assert_eq!(notes(&lines)[0], "8 updates available");
        assert_eq!(
            fields(&lines),
            [
                ("System".to_owned(), "4 updates".to_owned()),
                ("Flatpaks".to_owned(), "2 updates".to_owned()),
                ("AUR".to_owned(), "1 update".to_owned()),
                (
                    "Firmware".to_owned(),
                    "1 device update · 2 excluded".to_owned()
                ),
            ]
        );
        assert!(
            notes(&lines).contains(&"A restart may be needed afterwards.".to_owned()),
            "{lines:?}"
        );
        let labels: Vec<&str> = buttons.iter().map(|b| b.label.as_str()).collect();
        assert_eq!(labels, ["Update now", "Full output", "Not now"]);
        assert_eq!(buttons[0].command, C::UpdateInstall);

        // Nothing to do is a sentence and a way out, with no Update to press.
        let updates = reviewing(vec![
            checked(SourceId::System, 0, 0),
            checked(SourceId::Firmware, 0, 18),
        ]);
        let (lines, buttons) = updates.panel();
        assert_eq!(notes(&lines)[0], "Everything is up to date");
        assert_eq!(fields(&lines)[1].1, "No device updates · 18 excluded");
        assert!(
            !commands(&updates).contains(&C::UpdateInstall),
            "{buttons:?}"
        );

        // A source that could not be checked says so in two words; the
        // reason is in Details, not on the review.
        let mut broken = checked(SourceId::Firmware, 0, 0);
        broken.error =
            Some("fwupd could not be reached over D-Bus · the daemon is not running".into());
        broken.executable = false;
        let updates = reviewing(vec![checked(SourceId::System, 3, 0), broken]);
        let (lines, _) = updates.panel();
        assert_eq!(fields(&lines)[1].1, "Could not check");
        assert_eq!(notes(&lines)[0], "3 updates available");
    }

    /// A machine whose package manager cannot list what it would change
    /// without root — APT before `apt-get update` — is not told it is up to
    /// date, and is still offered the update that refreshes.
    #[test]
    fn a_stale_list_is_not_up_to_date() {
        let mut apt = checked(SourceId::System, 0, 0);
        apt.fresh = false;
        let updates = reviewing(vec![apt]);
        let (lines, _) = updates.panel();
        assert_eq!(notes(&lines)[0], "Nothing new since the last refresh");
        assert_eq!(fields(&lines)[0].1, "None since the last refresh");
        assert!(commands(&updates).contains(&menu::Command::UpdateInstall));

        // And one that cannot count at all is ready, not empty.
        let mut ostree = checked(SourceId::System, 0, 0);
        ostree.listed = false;
        let updates = reviewing(vec![ostree]);
        let (lines, _) = updates.panel();
        assert_eq!(notes(&lines)[0], "Ready to update");
        assert!(commands(&updates).contains(&menu::Command::UpdateInstall));
    }

    /// Every source a machine can have, on one review, fits the smallest
    /// display the shell is drawn on — with no paging, because eight fields
    /// is what a review of eight sources is.
    #[test]
    fn a_review_of_every_source_fits_the_display() {
        let updates = reviewing(
            [
                SourceId::System,
                SourceId::Flatpak,
                SourceId::Aur,
                SourceId::Snap,
                SourceId::Nix,
                SourceId::Guix,
                SourceId::AppImage,
                SourceId::Firmware,
            ]
            .into_iter()
            .map(|id| checked(id, 1, 1))
            .collect(),
        );
        let (lines, buttons) = updates.panel();
        assert_eq!(fields(&lines).len(), 8);
        assert!(!commands(&updates).contains(&menu::Command::UpdateOverview));
        let last = buttons.len() - 1;
        let mut dialog = crate::dialog::Dialog::default();
        assert!(dialog.ask([0.0; 4], None, lines, buttons, 0));
        let (width, height) = (1280.0, 720.0);
        let panel = crate::ui::dialog_rect(width, height, &dialog);
        let button = crate::ui::dialog_button_rect(width, height, &dialog, last).unwrap();
        assert!(panel[1] >= 0.0, "panel above the display");
        assert!(
            button[1] + button[3] <= panel[1] + panel[3] && panel[1] + panel[3] <= height,
            "last button outside: {button:?} in {panel:?}"
        );
    }

    /// While the tools run there is a bar, and it counts things across
    /// sources: the finished source's items, then what the active tool says
    /// it is on. A question at the end of the output is a Yes and a No, and
    /// once answered it is not asked again by the echo of the answer.
    #[test]
    fn a_running_job_is_a_bar_and_a_question_is_two_buttons() {
        use menu::Command as C;
        let (send, work) = mpsc::channel();
        let (_done, receive) = mpsc::channel();
        let mut updates = Updates::over(send, receive);
        updates.open = true;
        updates.snapshot.phase = Phase::Running;
        updates.snapshot.job = 3;
        updates.snapshot.sources = vec![
            checked(SourceId::System, 4, 0),
            checked(SourceId::Flatpak, 2, 0),
        ];
        updates.snapshot.selected = vec![SourceId::System, SourceId::Flatpak];
        updates.snapshot.active = Some(SourceId::System);
        updates.snapshot.output = vec![
            "— System —".into(),
            "pacman".into(),
            ":: Synchronizing package databases...".into(),
            "(4/4) checking keys in keyring".into(),
            ":: Proceed with installation? [Y/n]".into(),
        ];
        let (lines, _) = updates.panel();
        assert_eq!(fields(&lines)[0].1, "System, 1 of 2");
        assert!(lines.contains(&Line::Progress(0)), "{lines:?}");
        assert!(
            notes(&lines).contains(&"0 of 6  ·  0%".to_owned()),
            "{lines:?}"
        );
        let (_, buttons) = updates.panel();
        let labels: Vec<&str> = buttons.iter().map(|b| b.label.as_str()).collect();
        assert_eq!(labels, ["Yes", "No", "Full output", "Run in background"]);

        updates.answer(true);
        let sent: Vec<Work> = work.try_iter().collect();
        assert!(matches!(sent[..], [Work::Input { job: 3, .. }]));
        updates.awaiting = false;
        // The echo: the same line with the answer after it is not a new
        // question. Full output is still there to read what was said.
        updates.snapshot.output.last_mut().unwrap().push_str(" y");
        assert!(!commands(&updates).contains(&C::UpdateYes));
        assert!(commands(&updates).contains(&C::UpdateOutput));
        assert!(updates
            .snapshot
            .output
            .last()
            .is_some_and(|l| asks_yes_or_no(l)));
        updates
            .snapshot
            .output
            .push(":: Proceed with installation? [Y/n]".into());
        assert!(
            commands(&updates).contains(&C::UpdateYes),
            "a later identical question still needs an answer"
        );

        updates.snapshot.output.extend([
            "(1/4) upgrading linux".into(),
            "(3/4) upgrading systemd".into(),
        ]);
        assert_eq!(updates.progress(), Some((2, 6)));
        // Hooks after the packages count something else and are not the bar.
        updates.snapshot.output.extend([
            "(4/4) upgrading vulkan-radeon".into(),
            "(1/12) Arming ConditionNeedsUpdate...".into(),
            "(2/12) Updating icon theme caches...".into(),
        ]);
        assert_eq!(updates.progress(), Some((3, 6)));
        // APT counts nothing; the packages it has set up are its measure.
        assert_eq!(
            counted_output(
                &[
                    "— System —".into(),
                    "Unpacking libc6 (2.40-4) over (2.40-3) ...".into(),
                    "Setting up libc6 (2.40-4) ...".into(),
                    "Setting up bash (5.2-2) ...".into(),
                ],
                SourceId::System,
                4
            ),
            Some((2, 4))
        );
        updates.snapshot.results.push(lxb_updates::ResultEntry {
            source: SourceId::System,
            note: "Finished".into(),
            success: true,
        });
        updates.snapshot.active = Some(SourceId::Flatpak);
        updates
            .snapshot
            .output
            .extend(["— Flatpaks —".into(), "Updating 2/2… 100%".into()]);
        assert_eq!(updates.progress(), Some((5, 6)));
        let (lines, _) = updates.panel();
        assert!(lines.contains(&Line::Progress(83)), "{lines:?}");
    }

    /// Progress with echo disabled must not ask for a password. A real prompt
    /// arriving later still opens the field once; Escape leaves it closed.
    #[test]
    fn a_password_prompt_opens_the_field_itself() {
        let (send, _work) = mpsc::channel();
        let (done, receive) = mpsc::channel();
        let mut updates = Updates::over(send, receive);
        updates.open = true;
        let answer = |secret: bool| {
            Ok(Response {
                snapshot: Snapshot {
                    job: 1,
                    phase: Phase::Running,
                    secret,
                    output: vec!["[sudo] password for someone:".into()],
                    ..Snapshot::default()
                },
                error: None,
                history: vec![],
                history_requested: false,
                log: None,
                events: vec![],
                daily_check: true,
            })
        };
        let mut progress = answer(true).unwrap();
        progress.snapshot.active = Some(SourceId::Flatpak);
        progress.snapshot.output = vec!["Updating 1/2… 66%".into()];
        done.send(Ok(progress)).unwrap();
        updates.poll(true);
        assert!(!updates.typing, "Flatpak progress is not authentication");
        assert!(!commands(&updates).contains(&menu::Command::UpdateRespond));
        assert!(!updates
            .panel()
            .0
            .iter()
            .any(|line| matches!(line, Line::Secret { .. })));

        done.send(answer(true)).unwrap();
        updates.poll(true);
        assert!(updates.typing);
        let (lines, buttons) = updates.panel();
        assert!(
            lines.iter().any(|l| matches!(l, Line::Secret { .. })),
            "{lines:?}"
        );
        assert!(notes(&lines).contains(&"Type the password it asks for.".to_owned()));
        assert_eq!(buttons[0].label, "Send");
        assert!(!commands(&updates).contains(&menu::Command::UpdateYes));

        assert!(updates.type_into(Stroke::ESCAPE));
        assert!(!updates.typing);
        done.send(answer(true)).unwrap();
        updates.poll(true);
        assert!(!updates.typing, "a field left is not reopened");
    }

    /// The finish is a sentence, a word a source, and the restart when one
    /// is owed — first, because it is the one thing left to do.
    #[test]
    fn the_finish_says_what_happened_and_offers_the_restart() {
        use menu::Command as C;
        let (send, _work) = mpsc::channel();
        let (_done, receive) = mpsc::channel();
        let mut updates = Updates::over(send, receive);
        updates.snapshot.phase = Phase::Completed;
        updates.snapshot.results = vec![
            lxb_updates::ResultEntry {
                source: SourceId::System,
                note: "Staged for the next boot · restart to finish".into(),
                success: true,
            },
            lxb_updates::ResultEntry {
                source: SourceId::Flatpak,
                note: "Finished · open applications keep the old version until restarted".into(),
                success: true,
            },
        ];
        updates.snapshot.restart = Some(lxb_updates::Restart::Normal);
        let (lines, buttons) = updates.panel();
        assert_eq!(notes(&lines)[0], "Updates installed.");
        assert_eq!(
            fields(&lines),
            [
                ("System".to_owned(), "Staged for the next boot".to_owned()),
                ("Flatpaks".to_owned(), "Finished".to_owned()),
            ]
        );
        assert_eq!(buttons[0].command, C::UpdateRestart);
        assert_eq!(buttons[0].label, "Restart now");

        // A failure is the word, and the tool's reason is kept for Details.
        updates.snapshot.phase = Phase::Failed;
        updates.snapshot.restart = None;
        updates.snapshot.results = vec![lxb_updates::ResultEntry {
            source: SourceId::System,
            note: "/usr/bin/pacman is not protected against replacement".into(),
            success: false,
        }];
        let (lines, buttons) = updates.panel();
        assert_eq!(notes(&lines)[0], "The updates could not be installed.");
        assert_eq!(fields(&lines)[0].1, "Failed");
        assert_eq!(buttons[0].command, C::UpdateOutput);
        // The reason is a word on the face of the panel and the whole of it
        // is in the transcript — there is nowhere else to look.
        updates.snapshot.output =
            vec!["/usr/bin/pacman is not protected against replacement".into()];
        updates.show_output();
        let (lines, _) = updates.panel();
        assert!(
            lines.iter().any(|line| matches!(
                line,
                Line::Terminal { lines: rows, .. }
                    if rows.iter().any(|r| r.contains("not protected against replacement"))
            )),
            "{lines:?}"
        );
    }

    /// Five hundred package names go into Full output, whole, where a
    /// scrolling frame can hold them — not into a panel that pages them ten
    /// at a time. Photographed the other way on 2026-09-15: fifty-nine pages
    /// of package names nobody was going to turn. The review opens its own
    /// transcript from the check it already has, so Full output answers on
    /// the press without waiting on the coordinator.
    #[test]
    fn full_output_carries_the_whole_list_the_review_is_about() {
        let mut system = checked(SourceId::System, 500, 0);
        system.provider = lxb_updates::Provider::System(lxb_updates::System::Pacman);
        for (i, item) in system.items.iter_mut().enumerate() {
            item.name = format!("libpackage{i}");
        }
        let mut updates = reviewing(vec![system, checked(SourceId::Firmware, 1, 18)]);
        updates.show_output();
        let rows = updates.output_lines();
        assert!(rows.iter().any(|l| l == "— System —"), "{:?}", &rows[..4]);
        // The first name and the five-hundredth, both of them, because the
        // frame scrolls and does not have to choose.
        assert!(rows.iter().any(|l| l.starts_with("libpackage0 ")));
        assert!(rows.iter().any(|l| l.starts_with("libpackage499 ")));
        assert_eq!(
            rows.iter().filter(|l| l.starts_with("libpackage")).count(),
            500
        );
        // Every excluded device says why, in the same place.
        assert_eq!(
            rows.iter().filter(|l| l.starts_with("Excluded:")).count(),
            18
        );
        // And the frame is still a frame: a window of it, not all of it.
        let (lines, _) = updates.panel();
        let shown = lines
            .iter()
            .find_map(|l| match l {
                Line::Terminal { lines, .. } => Some(lines.len()),
                _ => None,
            })
            .expect("a terminal frame");
        assert_eq!(shown, TERMINAL_ROWS);
    }

    /// Full output is the whole transcript in a terminal frame: opened
    /// while a tool runs it shows the last rows and keeps showing the last
    /// rows as the tool says more — until it is scrolled by hand, after
    /// which it stays where it was put, and follows again when scrolled
    /// back to the end. Photographed the other way on 2026-09-15: a page of
    /// ten wrapped lines from a sixty-line tail.
    #[test]
    fn full_output_is_the_whole_transcript_and_follows_until_scrolled() {
        let (send, _work) = mpsc::channel();
        let (_done, receive) = mpsc::channel();
        let mut updates = Updates::over(send, receive);
        updates.snapshot.phase = Phase::Running;
        let line = |i: usize| format!("line {i}");
        updates.snapshot.output = (0..TERMINAL_ROWS + 2).map(line).collect();
        updates.show_output();
        let frame = |updates: &Updates| -> (Vec<String>, String) {
            let (lines, _) = updates.panel();
            lines
                .iter()
                .find_map(|l| match l {
                    Line::Terminal { lines, rows, foot } => {
                        assert_eq!(*rows, TERMINAL_ROWS);
                        Some((lines.clone(), foot.clone()))
                    }
                    _ => None,
                })
                .expect("a terminal frame")
        };
        let (shown, foot) = frame(&updates);
        assert_eq!(shown.len(), TERMINAL_ROWS);
        assert_eq!(shown[0], line(2));
        assert_eq!(shown[TERMINAL_ROWS - 1], line(TERMINAL_ROWS + 1));
        assert!(foot.contains("following"), "{foot}");

        updates
            .snapshot
            .output
            .extend((0..TERMINAL_ROWS).map(|i| line(100 + i)));
        let (shown, _) = frame(&updates);
        assert_eq!(shown.last().unwrap(), &line(100 + TERMINAL_ROWS - 1));
        assert_eq!(updates.scroll, Scroll::Following);

        assert!(updates.scroll(false));
        assert!(matches!(updates.scroll, Scroll::At(_)));
        let (held, foot) = frame(&updates);
        assert!(foot.contains("Left and Right"), "{foot}");
        updates
            .snapshot
            .output
            .extend((0..TERMINAL_ROWS).map(|i| line(200 + i)));
        assert_eq!(frame(&updates).0, held, "scrolling is holding");
        assert!(updates.scroll(true));
        assert!(updates.scroll(true));
        assert!(updates.scroll(true));
        assert_eq!(
            updates.scroll,
            Scroll::Following,
            "scrolled back to the end, it follows again"
        );

        // The clear way back to live, whatever it was pressed with: the
        // Latest output button, or End on a keyboard. One step, from
        // wherever the frame was left.
        assert!(updates.scroll(false));
        assert!(updates.scroll(false));
        assert!(matches!(updates.scroll, Scroll::At(_)));
        updates.follow_output();
        assert_eq!(updates.scroll, Scroll::Following);
        assert_eq!(
            frame(&updates).0.last().unwrap(),
            &line(200 + TERMINAL_ROWS - 1)
        );

        updates.show_overview();
        assert!(!updates.scroll(true), "nothing to scroll on the job's view");
    }

    /// A line longer than the terminal was wide is folded onto the rows it
    /// took there, never cut; and the frame is eighty columns wide.
    #[test]
    fn a_long_line_is_folded_at_the_terminals_width() {
        let long: String = (0..COLUMNS * 2 + 5)
            .map(|i| char::from(b'a' + (i % 26) as u8))
            .collect();
        let rows = terminal_rows(&["short".into(), long.clone(), "  ".into()]);
        assert_eq!(rows.len(), 5, "{rows:?}");
        assert_eq!(rows[0], "short");
        assert_eq!(rows[1].chars().count(), COLUMNS);
        assert_eq!(rows[2].chars().count(), COLUMNS);
        assert_eq!(rows[3].chars().count(), 5);
        assert_eq!(rows[1..4].concat(), long);
        assert_eq!(rows[4], "", "a blank line is a blank row");
    }

    #[test]
    fn update_prompt_buttons_fit_above_the_keyboard() {
        let mut updates = Updates::default();
        updates.snapshot.phase = Phase::Running;
        updates.snapshot.selected = vec![SourceId::System, SourceId::Flatpak, SourceId::Firmware];
        updates.snapshot.active = Some(SourceId::Flatpak);
        updates.snapshot.output = vec!["A long native prompt that will occupy several lines in the output area before asking you to confirm its transaction [y/N]".into()];
        updates.typing = true;
        updates.error = Some("The previous response could not be delivered".into());
        let (lines, buttons) = updates.panel();
        let last = buttons.len() - 1;
        let mut dialog = crate::dialog::Dialog::default();
        dialog.ask([0.0; 4], None, lines, buttons, 0);
        for height in [720.0, 1080.0, 1440.0] {
            let width = height * 16.0 / 9.0;
            dialog.set_footer(height * 0.5);
            let panel = crate::ui::dialog_rect(width, height, &dialog);
            let button = crate::ui::dialog_button_rect(width, height, &dialog, last).unwrap();
            assert!(
                button[1] + button[3] <= panel[1] + panel[3],
                "last button outside panel at {height}"
            );
            assert!(
                button[1] + button[3] <= height * 0.5,
                "last button overlaps keyboard at {height}"
            );
        }
    }

    /// The terminal frame — twenty rows, its foot, and the buttons under
    /// it — stands inside the display at 720p, whether it is a running
    /// job's (which carries Respond, for a prompt the panel did not
    /// recognise as one) or a review's own transcript. No keyboard is ever
    /// under this view, so it is only the display it has to fit.
    #[test]
    fn the_terminal_frame_fits_the_display_however_it_was_opened() {
        let (send, _work) = mpsc::channel();
        let (_done, receive) = mpsc::channel();
        let mut updates = Updates::over(send, receive);
        updates.snapshot.phase = Phase::Running;
        updates.snapshot.output = (0..40)
            .map(|i| format!("line {i} of a transcript long enough to wrap onto a second line"))
            .collect();
        updates.show_output();
        let (lines, buttons) = updates.panel();
        assert!(
            lines.iter().any(|l| matches!(l, Line::Terminal { .. })),
            "{lines:?}"
        );
        // Respond is here and not on the panel behind it: the tool is
        // running and has not asked anything this panel recognised, and
        // somebody reading the raw output is the one who can tell.
        assert_eq!(
            buttons.iter().map(|b| b.label.as_str()).collect::<Vec<_>>(),
            ["Respond", "Back", "Run in background"]
        );
        let fits = |lines: Vec<Line>, buttons: Vec<menu::Entry>, what: &str| {
            let last = buttons.len() - 1;
            let mut dialog = crate::dialog::Dialog::default();
            assert!(dialog.ask([0.0; 4], None, lines, buttons, 0));
            for height in [720.0, 1080.0] {
                let width = height * 16.0 / 9.0;
                let panel = crate::ui::dialog_rect(width, height, &dialog);
                let button = crate::ui::dialog_button_rect(width, height, &dialog, last).unwrap();
                assert!(panel[1] >= 0.0, "{what} above the display at {height}");
                assert!(
                    button[1] + button[3] <= panel[1] + panel[3] && panel[1] + panel[3] <= height,
                    "{what}: last button outside at {height}: {button:?} in {panel:?}"
                );
                assert!(
                    panel[0] >= 0.0 && panel[0] + panel[2] <= width,
                    "{what} wider than the display at {height}: {panel:?}"
                );
            }
        };
        fits(lines, buttons, "the terminal frame");

        // A review's own transcript — three sources, eighteen exclusions,
        // more rows than the frame holds — is the tallest the frame gets,
        // and it is a fixed twenty rows however long the transcript is.
        let mut updates = reviewing(vec![
            checked(SourceId::System, 4, 0),
            checked(SourceId::Flatpak, 2, 0),
            checked(SourceId::Firmware, 1, 18),
        ]);
        updates.show_output();
        let (lines, buttons) = updates.panel();
        assert!(
            updates.output_lines().len() > TERMINAL_ROWS,
            "the frame is meant to be scrolled here"
        );
        assert_eq!(
            buttons.iter().map(|b| b.label.as_str()).collect::<Vec<_>>(),
            ["Back", "Close"]
        );
        fits(lines, buttons, "a full transcript");
    }

    /// The panel set aside for a question `polkitd` asked is not put away
    /// by the door every panel goes out through, and comes back open, on
    /// the view and at the place it was on.
    #[test]
    fn a_panel_set_aside_for_a_question_is_still_open() {
        let (send, _work) = mpsc::channel();
        let (_done, receive) = mpsc::channel();
        let mut updates = Updates::over(send, receive);
        updates.snapshot.phase = Phase::Running;
        updates.snapshot.output = (0..TERMINAL_ROWS * 3)
            .map(|i| format!("line {i}"))
            .collect();
        updates.press(UpdateValue::Everything);
        updates.buttons = vec![menu::Command::Dismiss];
        updates.show_output();
        assert!(updates.scroll(false));
        let place = updates.scroll;
        updates.edit();

        updates.set_aside();
        assert!(updates.aside && updates.open);
        assert!(!updates.typing, "the field was the tool's, and it has gone");
        assert!(updates.buttons.is_empty());
        updates.hide();
        assert!(
            updates.open,
            "the question's panel closing is not this one closing"
        );
        assert_eq!(updates.view, View::Output);
        assert_eq!(updates.scroll, place);

        updates.aside = false;
        updates.hide();
        assert!(!updates.open);
    }

    /// A press is answered on the spot with a wait, and until the coordinator
    /// answers it, nothing on the panel can be pressed but Close — not the
    /// Update it was showing a moment ago, which would be a second Install
    /// the coordinator refuses as "the review changed", a refusal of the
    /// thing the user just did. A row pressed meanwhile asks what is going
    /// on rather than starting a check beside the one already asked for.
    #[test]
    fn a_press_is_answered_with_a_wait_and_the_next_one_is_spent() {
        use menu::Command as C;
        let (send, work) = mpsc::channel();
        let (done, receive) = mpsc::channel();
        let mut updates = Updates::over(send, receive);
        updates.open = true;
        updates.snapshot.phase = Phase::Reviewing;
        updates.snapshot.job = 7;
        updates.snapshot.selected = vec![SourceId::System];
        updates.snapshot.sources = vec![checked(SourceId::System, 1, 0)];
        assert!(commands(&updates).contains(&C::UpdateInstall));

        updates.install();
        let (lines, buttons) = updates.panel();
        assert!(lines.contains(&Line::Waiting), "{lines:?}");
        assert_eq!(
            buttons.iter().map(|e| e.command).collect::<Vec<_>>(),
            [C::Dismiss]
        );

        updates.press(UpdateValue::Everything);
        let sent: Vec<Request> = work
            .try_iter()
            .map(|w| match w {
                Work::Request(r) => r,
                Work::Input { .. } => panic!("nothing was typed"),
            })
            .collect();
        assert!(
            matches!(sent[..], [Request::Install { job: 7 }, Request::Status]),
            "{sent:?}"
        );

        let answer = |phase: Phase| {
            Ok(Response {
                snapshot: Snapshot {
                    job: 7,
                    phase,
                    ..Snapshot::default()
                },
                error: None,
                history: vec![],
                history_requested: false,
                log: None,
                events: vec![],
                daily_check: true,
            })
        };
        done.send(answer(Phase::Running)).unwrap();
        updates.poll(true);
        // One of the two is still out; the wait holds.
        assert_eq!(commands(&updates), [C::Dismiss]);
        done.send(answer(Phase::Running)).unwrap();
        updates.poll(true);
        assert!(!updates.awaiting);
        assert!(commands(&updates).contains(&C::UpdateOutput));
    }

    /// A blocking condition is a plain sentence on the panel and the whole
    /// of it in Full output.
    ///
    /// The coordinator writes a notice as `headline · detail`. The detail is
    /// a mount path and a number of megabytes, which is technical and goes
    /// where everything technical goes; the headline is what somebody about
    /// to press Update needs to read. Two at most, so the panel stays a
    /// thing to read rather than the report this page was rebuilt to stop
    /// being.
    #[test]
    fn a_warning_is_a_plain_sentence_and_never_more_than_two() {
        let mut updates = reviewing(vec![checked(SourceId::System, 4, 0)]);
        updates.open = true;
        updates.snapshot.notices = vec![
            "Storage is nearly full · /boot has 173 MiB free; the native manager \
             checks whether the transaction fits"
                .into(),
            "Running on battery · connect power for long system or firmware updates".into(),
            "Keep this session open until the update finishes · the coordinator is \
             detached and has no user-service supervisor"
                .into(),
        ];
        let said = notes(&updates.panel().0).join(" ");
        assert!(said.contains("Storage is nearly full"), "{said}");
        assert!(said.contains("Running on battery"), "{said}");
        // The detail is not on the face of the panel.
        assert!(!said.contains("173 MiB"), "{said}");
        assert!(!said.contains("user-service supervisor"), "{said}");
        // And the third is not either — two is the cap.
        assert!(!said.contains("Keep this session open"), "{said}");

        // On the way out of a job, the same rule: a log that could not be
        // written is not a package that failed, so it is a warning and not
        // the outcome.
        updates.snapshot.phase = Phase::Completed;
        updates.snapshot.results = vec![lxb_updates::ResultEntry {
            source: SourceId::System,
            note: "Finished".into(),
            success: true,
        }];
        updates.snapshot.notices =
            vec!["Cannot record full output: no space left on device".into()];
        let said = notes(&updates.panel().0).join(" ");
        assert!(said.contains("Updates installed."), "{said}");
        assert!(said.contains("Cannot record full output"), "{said}");
    }

    /// The last thing a tool said before it finished is fetched.
    ///
    /// The frame asks for more while the job is running and stops when it
    /// stops — but `total` is only ever as new as the last chunk that came
    /// back, so a job that wrote its final lines and then ended left them on
    /// the coordinator's disk with the frame believing it had everything.
    /// One more ask after the job stops, and then no more.
    #[test]
    fn the_end_of_a_transcript_is_fetched_after_the_job_stops() {
        let (send, work) = mpsc::channel();
        let (done, receive) = mpsc::channel();
        let mut updates = Updates::over(send, receive);
        updates.snapshot.job = 9;
        updates.snapshot.phase = Phase::Running;
        updates.show_output();
        let requests = |work: &mpsc::Receiver<Work>| -> Vec<Request> {
            work.try_iter()
                .map(|w| match w {
                    Work::Request(r) => r,
                    Work::Input { .. } => panic!("nothing was typed"),
                })
                .collect()
        };
        let reply = |phase: Phase, chunk: Option<lxb_updates::journal::Chunk>| {
            Ok(Response {
                snapshot: Snapshot {
                    job: 9,
                    phase,
                    ..Snapshot::default()
                },
                error: None,
                history: vec![],
                history_requested: false,
                log: chunk,
                events: vec![],
                daily_check: true,
            })
        };
        let chunk = |offset: u64, bytes: &str, total: u64| lxb_updates::journal::Chunk {
            job: 9,
            offset,
            next: offset + bytes.len() as u64,
            total,
            bytes: bytes.as_bytes().to_vec(),
        };

        // Opening asked for the transcript from nought.
        assert!(matches!(
            requests(&work)[..],
            [Request::Output { job: 9, offset: 0 }]
        ));
        // Everything the coordinator knew of at the time, job still running.
        done.send(reply(Phase::Running, Some(chunk(0, "first\n", 6))))
            .unwrap();
        updates.poll(true);
        assert!(!updates.log_settled, "a running job is never settled");

        // The job ends, and the poll that noticed is the one carrying no
        // chunk — so the two lines it wrote on the way out are still out.
        done.send(reply(Phase::Completed, None)).unwrap();
        updates.poll(true);
        updates.next = Instant::now();
        updates.poll(true);
        let asked = requests(&work);
        assert!(
            asked
                .iter()
                .any(|r| matches!(r, Request::Output { job: 9, offset: 6 })),
            "the tail was never asked for: {asked:?}"
        );

        done.send(reply(Phase::Completed, Some(chunk(6, "last\n", 11))))
            .unwrap();
        updates.poll(true);
        assert!(updates.log_settled);
        assert_eq!(updates.output_lines(), ["first", "last"]);

        // Settled, so the frame stops asking rather than polling a finished
        // job for ever.
        updates.next = Instant::now();
        updates.poll(true);
        assert!(
            !requests(&work)
                .iter()
                .any(|r| matches!(r, Request::Output { .. })),
            "a settled transcript is not asked for again"
        );
    }

    /// Recent updates gains the attempt that just ended, while it is open.
    ///
    /// The list is asked for once, when the view opens. A job that ends
    /// behind it — one left to run in the background, say — would otherwise
    /// leave the newest attempt off the list until somebody left the view
    /// and came back.
    #[test]
    fn recent_updates_picks_up_a_job_that_ended_behind_it() {
        let (send, work) = mpsc::channel();
        let (done, receive) = mpsc::channel();
        let mut updates = Updates::over(send, receive);
        updates.snapshot.job = 4;
        updates.snapshot.phase = Phase::Running;
        updates.press(UpdateValue::History);
        let requests = |work: &mpsc::Receiver<Work>| -> Vec<Request> {
            work.try_iter()
                .map(|w| match w {
                    Work::Request(r) => r,
                    Work::Input { .. } => panic!("nothing was typed"),
                })
                .collect()
        };
        assert!(requests(&work)
            .iter()
            .any(|r| matches!(r, Request::History)));

        let ended = Snapshot {
            job: 4,
            phase: Phase::Completed,
            ..Snapshot::default()
        };
        done.send(Ok(Response {
            snapshot: ended.clone(),
            error: None,
            history: vec![],
            history_requested: false,
            log: None,
            events: vec![],
            daily_check: true,
        }))
        .unwrap();
        updates.poll(true);
        updates.poll(true);
        assert!(
            requests(&work)
                .iter()
                .any(|r| matches!(r, Request::History)),
            "the list was not asked for again"
        );

        // And having asked, it does not ask on every turn of the loop.
        updates.poll(true);
        assert!(!requests(&work)
            .iter()
            .any(|r| matches!(r, Request::History)));
    }

    /// The rows of the column say the state of each source in a few words,
    /// and the list of what is waiting is theirs to carry — only when the
    /// source really listed it.
    #[test]
    fn rows_carry_a_state_and_a_list() {
        let mut unlisted = checked(SourceId::Flatpak, 3, 0);
        unlisted.listed = false;
        let sources = [
            checked(SourceId::System, 2, 0),
            unlisted,
            checked(SourceId::Firmware, 0, 1),
        ];
        let rows: Vec<Row> = sources
            .iter()
            .map(|source| Row {
                id: source.id,
                note: summary(source),
                items: if source.listed {
                    source.items.clone()
                } else {
                    vec![]
                },
            })
            .collect();
        assert_eq!(rows[0].note, "2 updates");
        assert_eq!(rows[0].items.len(), 2);
        assert_eq!(rows[1].note, "Ready to update");
        assert!(rows[1].items.is_empty());
        assert_eq!(rows[2].note, "No device updates · 1 excluded");
        assert_eq!(Tally::of(sources.iter()).headline(), "2 updates available");
        assert_eq!(Tally::of(std::iter::empty()).headline(), "Not checked yet");
    }

    #[test]
    fn native_prompts_wrap_without_losing_text_or_splitting_utf8() {
        let input = "A package manager can ask whether to replace a dependency: 確認してください";
        let lines = wrap(input, 46);
        assert!(lines.iter().all(|l| l.chars().count() <= 46));
        assert_eq!(lines.join(" "), input);
        let long = "界".repeat(120);
        assert_eq!(wrap(&long, 46).concat(), long);
    }

    #[test]
    fn the_questions_tools_ask_are_recognised() {
        for line in [
            ":: Proceed with installation? [Y/n]",
            "Do you want to continue? [Y/n] ",
            "Is this ok [y/N]: ",
            "Continue? [y/n/v/...? shows all options] (y): ",
            "Would you like to merge these packages? [Yes/No] ",
            "Perform operation? [Y|n]: ",
            "Proceed with these changes to the user installation? [Y/n]: ",
        ] {
            assert!(asks_yes_or_no(line), "{line}");
        }
        for line in [
            "Enter a number (default=1): ",
            "[sudo] password for someone:",
            "(2/4) upgrading mesa",
        ] {
            assert!(!asks_yes_or_no(line), "{line}");
        }
        assert_eq!(fraction("(2/4) upgrading mesa [####] 100%"), Some((2, 4)));
        assert_eq!(fraction("Updating 1/1… 100%"), Some((1, 1)));
        assert_eq!(fraction("no count here 100%"), None);
        assert_eq!(fraction("Upgrading  : bash-5.2  3/8"), Some((3, 8)));
    }
}
