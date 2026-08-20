//! Stopping an application nobody can see, and starting it again.
//!
//! Withholding frames takes a covered application down to no frames at all —
//! see [`crate::render::post_repaint`] — and that is the whole saving as far as
//! the screen is concerned. It is not the whole saving as far as the *room* is
//! concerned, which is the complaint this exists for: a game behind the start
//! screen went on playing its music, because music is not frames. It is a
//! thread of its own, feeding an audio device that has no idea what is on the
//! display, and no amount of not being asked to draw will stop it. The same
//! goes for a simulation running ahead of its renderer, a download, a chat
//! client's timers, and every other thing a program does that is not painting.
//!
//! So a console does what a console does: it stops the process. `SIGSTOP` on
//! everything the application is, `SIGCONT` when it comes back — the same tree
//! [`crate::teardown`] works out for Close, since "what is this application"
//! has exactly one answer and it is written down there.
//!
//! ## What this costs, deliberately
//!
//! A stopped process is *stopped*: its clock does not advance, its sockets are
//! not read, and anything on the other end of one of them is talking to
//! nobody. An online game will be disconnected. A download stops until the
//! screen comes back. This was chosen over the alternatives — muting the
//! application, or asking it nicely — because it is the only one that is true
//! of everything a program can be doing while nobody is looking at it, and
//! because it is what the user asked for when they were shown the choice.
//!
//! ## What is never stopped
//!
//! * **Valve's client**, and any other supervisor's own interface. It starts
//!   games, so a session that stopped it would be one where the next press of a
//!   game did nothing at all. [`crate::teardown::sleeping`] holds that rule.
//! * **A window the shell is driving out of sight.** The shell is waiting on
//!   it — that is what being out of sight means here — and it is not on screen
//!   precisely because the shell put it where nobody would see it.
//! * **A window on no display at all.** Clients park windows off the edge of
//!   the world and move them on when they want them seen; not being anywhere is
//!   not the same as being covered, and this stops only what it is sure about.
//! * **Anything sharing a process tree with a window that is on screen.** Two
//!   windows of one program can carry different pids, and a tree that reaches
//!   one somebody is looking at is not a tree to stop.
//! * **An application the shell says is playing something.** The complaint
//!   above is a game going on playing its music into the room; the answer to it
//!   must not also stop the music somebody *put on*. A player is a program
//!   whose whole purpose is what it is doing out of sight, and stopping one is
//!   the same fault as not stopping the game — the same silence, arrived at
//!   from the other side. Which of the two a sound is cannot be seen from here,
//!   so the shell is asked: see [`LxbState::keep_application_awake`] and
//!   `lxb_shell_v1.keep_awake`.
//!
//! And nothing stays stopped through a teardown: see
//! [`LxbState::wake_every_sleeping_application`], which the session's own exit
//! goes through. A process left stopped by a compositor that is no longer there
//! is a process nobody will ever continue.

use std::collections::{HashMap, HashSet};

use smithay::desktop::Window;
use smithay::output::Output;

use crate::state::LxbState;
use crate::teardown::{self, Boundary, Doomed, Processes};

/// Which applications are stopped, and which were looked at and left alone.
#[derive(Debug, Default)]
pub struct Sleepers {
    /// The processes stopped for each window process, so waking one needs no
    /// second walk of `/proc` — and so a tree read while the application was
    /// whole is the tree that gets continued.
    asleep: HashMap<i32, Asleep>,
    /// Window processes already found not to be stoppable, so the answer is
    /// worked out once rather than on every frame the shell draws. Forgotten
    /// whenever what is on screen changes, since that is what the answer can
    /// depend on.
    refused: HashSet<i32>,
    /// What was on screen when `refused` was last filled in.
    last_shown: HashSet<i32>,
    /// Applications that have been asked to end. They are never stopped again,
    /// however invisible they become: a `SIGTERM` is *queued* for a stopped
    /// process and acted on only when it is continued, so an application
    /// stopped again a frame after Close would sit there holding everything it
    /// held until the grace ran out and it was killed outright — which is the
    /// opposite of what asking first is for.
    ending: HashSet<i32>,
}

#[derive(Debug)]
struct Asleep {
    /// What the application calls itself, for the log. A stopped process that
    /// nobody can name is the hardest kind of bug to read afterwards.
    app_id: String,
    processes: Vec<Doomed>,
}

impl LxbState {
    /// Stop every application with nothing on screen, and continue every one
    /// that has something again.
    ///
    /// Asked wherever the picture can have changed — the same two places
    /// [`LxbState::refresh_window_activation`] is asked from, and for the same
    /// reason: what an application is *told* about being on screen and whether
    /// it is allowed to run have to be the same answer, or a game is stopped
    /// while it is being played.
    ///
    /// Cheap when nothing changed. Two sets are built from state already in
    /// hand, and `/proc` is read only when there is something new to stop.
    pub(crate) fn refresh_application_sleep(&mut self) {
        let outputs: Vec<Output> = self.lxb.space.outputs().cloned().collect();
        let mut shown: HashSet<i32> = HashSet::new();
        for output in &outputs {
            for window in crate::render::windows_on_screen(&self.lxb, output) {
                shown.extend(self.window_pid(&window));
            }
        }

        // Everything that is on a display and has nothing of itself on screen.
        let windows: Vec<Window> = self.lxb.space.elements().cloned().collect();
        let hidden: Vec<(i32, Window)> = windows
            .into_iter()
            .filter(|window| !self.lxb.space.outputs_for_element(window).is_empty())
            .filter(|window| !self.lxb.out_of_sight(window))
            // Left out rather than refused, so that one filter does both
            // halves: an application that starts playing while it is stopped
            // drops out of `hidden` here and is continued below by
            // `wake_what_can_be_seen_again`, which is the same thing that
            // happens when its window comes back on screen.
            .filter(|window| !self.lxb.media_is_playing(window))
            .filter_map(|window| Some((self.window_pid(&window)?, window)))
            .filter(|(pid, _)| !shown.contains(pid))
            .collect();

        self.wake_what_can_be_seen_again(&hidden);

        // A refusal can depend on what else is on screen, so it is only
        // remembered for as long as that holds still.
        if self.lxb.sleepers.last_shown != shown {
            self.lxb.sleepers.refused.clear();
            self.lxb.sleepers.last_shown = shown.clone();
        }

        let fresh: Vec<(i32, Window)> = hidden
            .into_iter()
            .filter(|(pid, _)| !self.lxb.sleepers.asleep.contains_key(pid))
            .filter(|(pid, _)| !self.lxb.sleepers.refused.contains(pid))
            .filter(|(pid, _)| !self.lxb.sleepers.ending.contains(pid))
            .collect();
        if fresh.is_empty() {
            return;
        }
        self.put_these_to_sleep(fresh, &shown);
    }

    /// Continue everything that is stopped and should not be — because it is on
    /// screen again, or because its window has gone.
    ///
    /// A window that has gone is the one worth spelling out. Its application
    /// may be closing, and a process stopped mid-exit never finishes exiting:
    /// it sits there, stopped, holding whatever it held. So anything this
    /// stopped is continued the moment it stops being a window on a display
    /// nobody can see, whichever way that happened.
    fn wake_what_can_be_seen_again(&mut self, still_hidden: &[(i32, Window)]) {
        let waking: Vec<i32> = self
            .lxb
            .sleepers
            .asleep
            .keys()
            .copied()
            .filter(|pid| !still_hidden.iter().any(|(hidden, _)| hidden == pid))
            .collect();
        for pid in waking {
            let Some(sleeping) = self.lxb.sleepers.asleep.remove(&pid) else {
                continue;
            };
            let woken = teardown::signal(&sleeping.processes, libc::SIGCONT);
            tracing::info!(
                app_id = %sleeping.app_id,
                pid,
                processes = woken,
                "an application is on screen again, and running again"
            );
        }
    }

    /// Work out what each of these applications is, and stop it.
    fn put_these_to_sleep(&mut self, fresh: Vec<(i32, Window)>, shown: &HashSet<i32>) {
        let processes = Processes::read();
        let boundary = Boundary {
            shell: self.lxb.session_shell_pid,
            compositor: Some(std::process::id() as i32),
        };
        for (pid, window) in fresh {
            let app_id = crate::shell_control::window_app_id(&window);
            let Some(doomed) = teardown::sleeping(&app_id, Some(pid), &processes, boundary) else {
                self.lxb.sleepers.refused.insert(pid);
                continue;
            };
            // The last guard, and the one that is about *this* moment rather
            // than about what an application is: a tree that reaches a window
            // somebody is looking at is not one to stop, whatever it is called.
            if doomed.iter().any(|process| shown.contains(&process.pid)) {
                tracing::debug!(
                    %app_id,
                    pid,
                    "not stopping this: its processes reach a window that is on screen"
                );
                self.lxb.sleepers.refused.insert(pid);
                continue;
            }
            let stopped = teardown::signal(&doomed, libc::SIGSTOP);
            if stopped == 0 {
                self.lxb.sleepers.refused.insert(pid);
                continue;
            }
            tracing::info!(
                %app_id,
                pid,
                processes = stopped,
                "nothing of this application is on screen, so it is stopped"
            );
            self.lxb.sleepers.asleep.insert(
                pid,
                Asleep {
                    app_id,
                    processes: doomed,
                },
            );
        }
    }

    /// The shell says an application is playing something, or has stopped.
    ///
    /// The one exception to all of this, and the only one there can be: what
    /// separates an album from a game's soundtrack is not in the window, the
    /// process tree or the audio device, and nothing this process can see
    /// tells the two apart. It is on the session bus — see
    /// `lxb_shell_v1.keep_awake` — so the shell works it out and this obeys.
    ///
    /// Both directions are the same question asked again, so both run the
    /// whole rule: an application that has just started playing is continued
    /// by [`LxbState::wake_what_can_be_seen_again`], because it has just
    /// dropped out of what counts as hidden, and one that has stopped is put
    /// to sleep by the same pass if it is still out of sight.
    pub(crate) fn keep_application_awake(&mut self, app_id: &str, awake: bool) {
        // Folded the way the window's own name will be, for the reason
        // `keep_out_of_sight` folds it. An empty name is refused rather than
        // stored: in the set it would spare every window whose client never
        // said what it was, which on an X11-heavy session is most of them.
        let Some(name) = crate::state::folded_app_id(app_id) else {
            tracing::debug!("the shell named an application with no name as playing");
            return;
        };
        let changed = if awake {
            self.lxb.playing.insert(name.clone())
        } else {
            self.lxb.playing.remove(&name)
        };
        if !changed {
            return;
        }
        tracing::info!(
            app_id = %name,
            awake,
            "the shell changed what may be stopped while nobody is looking"
        );
        self.refresh_application_sleep();
    }

    /// Start one application again because it is about to be asked to do
    /// something — and remember that it must not be stopped again.
    ///
    /// Close is the whole of it, in both its forms. A window is asked politely
    /// first and signalled if it does not go, and a stopped process can answer
    /// neither: the request sits unread in a socket nobody is reading, the
    /// signal sits pending on a process that will never run to handle it, and
    /// what the user sees is an application that ignored Close. So the way out
    /// of a stopped application is opened before it is asked to take it.
    pub(crate) fn wake_this_application(&mut self, window: &Window) {
        let Some(pid) = self.window_pid(window) else {
            return;
        };
        self.lxb.sleepers.ending.insert(pid);
        let Some(sleeping) = self.lxb.sleepers.asleep.remove(&pid) else {
            return;
        };
        let woken = teardown::signal(&sleeping.processes, libc::SIGCONT);
        tracing::info!(
            app_id = %sleeping.app_id,
            pid,
            processes = woken,
            "starting a stopped application again so it can be closed"
        );
    }

    /// Continue everything, whatever the screen says.
    ///
    /// The one call that must never be forgotten. A stopped process outlives
    /// the compositor that stopped it — `SIGCONT` is the only thing that undoes
    /// `SIGSTOP`, and a session that has exited sends no signals — so a game
    /// left stopped here is a game the user has to find with a process list.
    /// Cheap and safe to call twice: continuing something that is running does
    /// nothing at all.
    pub(crate) fn wake_every_sleeping_application(&mut self, why: &str) {
        let sleeping = std::mem::take(&mut self.lxb.sleepers.asleep);
        if sleeping.is_empty() {
            return;
        }
        for (pid, application) in sleeping {
            let woken = teardown::signal(&application.processes, libc::SIGCONT);
            tracing::info!(
                app_id = %application.app_id,
                pid,
                processes = woken,
                why,
                "starting a stopped application again"
            );
        }
    }
}
