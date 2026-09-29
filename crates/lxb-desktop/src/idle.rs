//! When nobody is using the machine: dimming, going dark, sleeping — and the
//! power button and the battery, which are the two other ways the machine's
//! power reaches the person holding it.
//!
//! A console's power story is short and everybody already knows it. Put it
//! down, and a little later the screen dims; a little after that it goes dark;
//! leave it long enough and it sleeps. Pick it up, and it is where it was. The
//! power button puts it to sleep, and holding it asks what to do. A battery
//! running low says so, twice, and a battery that is about to run out puts the
//! machine to sleep before it dies with a game open. That is the whole of what
//! this module does, and Settings > Power is the whole of how it is set.
//!
//! ## Who decides what
//!
//! The *decisions* are made here, because this is the half of the session that
//! knows about the controller — the compositor never sees a pad — and the half
//! that has the settings. The *timing* is the compositor's, over the standard
//! ext-idle-notify-v1: three notifications are kept, one per wait, and the
//! compositor says when each has run out and when somebody came back. A pad
//! press is passed down to it as `lxb_shell_v1.user_active`, so its timers are
//! about the person and not about which device the person is holding.
//!
//! ## What holds what
//!
//! **The screen** — dim and dark — is held by a program asking for it: a
//! Wayland film player's idle inhibitor, which the compositor honours only
//! while that window can be seen, and a program that asked over the session
//! bus (`org.freedesktop.ScreenSaver`, or the desktop portal's Inhibit for a
//! program in a sandbox), which is honoured while any application is in front
//! — see [`crate::power_bus`].
//!
//! **Sleep** is held by all of that too — a game in front that asked for the
//! screen to stay on keeps the machine awake as well — and by what would
//! really be lost by sleeping behind the start screen: something *playing*
//! (music, a film — the MPRIS players the guide's card shows), a Steam
//! download, an update being installed, files being copied, a program that
//! asked over `org.freedesktop.PowerManagement.Inhibit`, and anything the
//! login manager has been asked to keep awake. When the last of those lets go
//! and the machine is still idle, it sleeps then. All three waits are made
//! with `get_idle_notification`, the kind the protocol lets inhibitors hold.
//!
//! None of it holds a sleep somebody *asked for*. The power menu and the power
//! button put the machine to sleep whatever its programs hold — see
//! [`crate::power_bus::PowerBus::sleep_now`] — and so does a battery about to
//! run out.
//!
//! ## A nested session
//!
//! A session inside another desktop shares that desktop's login manager, so
//! it must not put the machine to sleep or take its power button — see
//! `LXB_SESSION_BACKEND`, which the compositor sets. The screens still dim and
//! go dark there, which is how any of this can be looked at without a laptop.

use std::time::{Duration, Instant};

use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::QueueHandle;
use wayland_protocols::ext::idle_notify::v1::client::ext_idle_notification_v1::ExtIdleNotificationV1;
use wayland_protocols::ext::idle_notify::v1::client::ext_idle_notifier_v1::ExtIdleNotifierV1;

use crate::settings::PowerSettings;

/// How long the power button has to be held before it opens the power menu
/// rather than doing what a press does. A second: a press is never that long,
/// and the firmware's own forced power-off is several times longer.
pub const HOLD_FOR_THE_MENU: Duration = Duration::from_secs(1);

/// How long after the machine wakes the power button is ignored.
///
/// Waking a machine with its power button hands the same press to userspace
/// on some firmware, and a press that arrives a moment after waking is the
/// press that woke it — answering it with Sleep would put the machine straight
/// back to sleep, which reads as a machine that will not wake up.
const AFTER_WAKING: Duration = Duration::from_secs(2);

/// How far a built-in panel's backlight is taken down when the screen dims: to
/// this much of wherever the person had it. A third reads plainly as dimmed and
/// still shows what is on it.
pub const DIM_TO: f32 = 0.3;

/// How often a controller in use is passed down to the compositor. Its timers
/// are minutes long; twice a second is plenty, and it is one message rather
/// than one per poll.
const ACTIVITY_EVERY: Duration = Duration::from_millis(500);

/// The three battery warnings, and the charge at which the power saver comes on
/// by itself. See [`battery_step`].
const LOW: u8 = 10;
const VERY_LOW: u8 = 5;
const EMPTY: u8 = 3;
const SAVER_AT: u8 = 20;

/// How long a battery that is about to run out is given, from the warning, to
/// be plugged in before the machine is put to sleep.
const EMPTY_GRACE: Duration = Duration::from_secs(15);

/// Which of the three waits a notification is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Timer {
    Dim,
    Off,
    Sleep,
}

/// What the screens are to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Screens {
    #[default]
    Awake,
    Dim,
    Off,
}

/// How long each wait is, in milliseconds, or `None` for a wait that is not
/// kept at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Waits {
    pub dim: Option<u32>,
    pub off: Option<u32>,
    pub sleep: Option<u32>,
}

/// The waits the settings ask for, on the battery or on the mains.
///
/// Dimming is left out where it would come at or after the screen going dark:
/// a screen dimmed a moment after it went out is a screen that never dims.
pub fn waits(settings: PowerSettings, on_battery: bool) -> Waits {
    let ms = |seconds: u32| (seconds > 0).then(|| seconds.saturating_mul(1000));
    let dims_first =
        settings.screen_off_after == 0 || settings.dim_after < settings.screen_off_after;
    Waits {
        dim: ms(settings.dim_after).filter(|_| dims_first),
        off: ms(settings.screen_off_after),
        sleep: ms(if on_battery {
            settings.sleep_on_battery
        } else {
            settings.sleep_plugged_in
        }),
    }
}

/// What the screens are to be, from which waits have run out.
pub fn screens(dim: bool, off: bool) -> Screens {
    match (dim, off) {
        (_, true) => Screens::Off,
        (true, false) => Screens::Dim,
        _ => Screens::Awake,
    }
}

/// Everything that holds sleep off besides what holds the screen, which holds
/// the sleep wait itself. See the module's notes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Holds {
    pub playing: bool,
    pub downloading: bool,
    pub copying: bool,
    pub asked: bool,
}

impl Holds {
    pub fn any(self) -> bool {
        self.playing || self.downloading || self.copying || self.asked
    }
}

/// What the battery warrants, one step at a time, given the last step taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatteryStep {
    /// Say it is low.
    Low,
    /// Say it is very low.
    VeryLow,
    /// Say it is about to run out, and sleep shortly unless it is plugged in.
    Empty,
}

/// The next warning a battery at `percent` warrants, or `None`. Each is given
/// once on the way down, and all of them are forgotten when the battery is
/// plugged in — a battery that dips to nine per cent twice in an evening is
/// told about twice.
pub fn battery_step(
    percent: u8,
    on_battery: bool,
    last: Option<BatteryStep>,
) -> Option<BatteryStep> {
    if !on_battery {
        return None;
    }
    let due = match percent {
        p if p <= EMPTY => BatteryStep::Empty,
        p if p <= VERY_LOW => BatteryStep::VeryLow,
        p if p <= LOW => BatteryStep::Low,
        _ => return None,
    };
    let rank = |step: BatteryStep| match step {
        BatteryStep::Low => 0,
        BatteryStep::VeryLow => 1,
        BatteryStep::Empty => 2,
    };
    match last {
        Some(last) if rank(last) >= rank(due) => None,
        _ => Some(due),
    }
}

/// Whether the power saver should be on by itself: the setting is on, the
/// battery is running the machine, and it is at or below [`SAVER_AT`]. Off
/// again the moment the charger goes in.
pub fn saver_wanted(enabled: bool, percent: u8, on_battery: bool) -> bool {
    enabled && on_battery && percent <= SAVER_AT
}

/// The ext-idle-notify side: one notification per wait, made again when a wait
/// changes.
pub struct Timers {
    notifier: ExtIdleNotifierV1,
    seat: WlSeat,
    dim: Option<(ExtIdleNotificationV1, u32)>,
    off: Option<(ExtIdleNotificationV1, u32)>,
    sleep: Option<(ExtIdleNotificationV1, u32)>,
}

impl Timers {
    pub fn new(notifier: ExtIdleNotifierV1, seat: WlSeat) -> Self {
        Timers {
            notifier,
            seat,
            dim: None,
            off: None,
            sleep: None,
        }
    }

    /// Make each notification match its wait. Returns the ones made or
    /// dropped, whose idle state starts over from that moment.
    pub fn sync<D>(&mut self, waits: Waits, qh: &QueueHandle<D>) -> Vec<Timer>
    where
        D: wayland_client::Dispatch<ExtIdleNotificationV1, Timer> + 'static,
    {
        let mut changed = Vec::new();
        for (timer, slot, wanted) in [
            (Timer::Dim, &mut self.dim, waits.dim),
            (Timer::Off, &mut self.off, waits.off),
            (Timer::Sleep, &mut self.sleep, waits.sleep),
        ] {
            if slot.as_ref().map(|(_, ms)| *ms) == wanted {
                continue;
            }
            if let Some((old, _)) = slot.take() {
                old.destroy();
            }
            if let Some(ms) = wanted {
                let made = self
                    .notifier
                    .get_idle_notification(ms, &self.seat, qh, timer);
                *slot = Some((made, ms));
            }
            changed.push(timer);
        }
        changed
    }
}

/// What the policy wants done this pass, for the shell to carry out.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Doing {
    /// The screens changed; apply this.
    pub screens: Option<Screens>,
    /// Put the machine to sleep now.
    pub sleep: bool,
    /// A battery warning to show.
    pub warn: Option<(BatteryStep, u8)>,
    /// Open the power menu: the button has been held.
    pub power_menu: bool,
}

/// The policy's state across passes.
#[derive(Default)]
pub struct Idle {
    pub timers: Option<Timers>,
    dim_idle: bool,
    off_idle: bool,
    sleep_idle: bool,
    /// What the screens were last set to, and when.
    applied: Screens,
    applied_at: Option<Instant>,
    /// When sleep was last asked for, so a refusal is retried rather than
    /// asked again every pass.
    sleep_asked: Option<Instant>,
    /// When the machine last woke — see [`AFTER_WAKING`].
    woke: Option<Instant>,
    /// When the power button went down, and whether the menu has already been
    /// opened for this press.
    button_down: Option<Instant>,
    menu_opened: bool,
    /// When the controller was last passed down — see [`ACTIVITY_EVERY`].
    last_activity: Option<Instant>,
    /// The last battery warning given on this discharge.
    warned: Option<BatteryStep>,
    /// When the battery was found about to run out.
    empty_since: Option<Instant>,
    /// Whether the policy has said, in the log, why it will not sleep. Said
    /// once per idle stretch rather than once a pass — and again when what
    /// holds sleep changes, which is the one thing worth a second line.
    said_why: bool,
    held: bool,
}

impl Idle {
    /// The compositor says a wait ran out (`idle`) or somebody came back.
    pub fn heard(&mut self, timer: Timer, idle: bool) {
        tracing::debug!(?timer, idle, "idle");
        match timer {
            Timer::Dim => self.dim_idle = idle,
            Timer::Off => self.off_idle = idle,
            Timer::Sleep => {
                self.sleep_idle = idle;
                if !idle {
                    self.sleep_asked = None;
                    self.said_why = false;
                }
            }
        }
    }

    /// Forget every idle state: a notification was made again, or the machine
    /// has just woken and everything is starting over.
    pub fn start_over(&mut self) {
        self.dim_idle = false;
        self.off_idle = false;
        self.sleep_idle = false;
        self.sleep_asked = None;
        self.said_why = false;
    }

    /// The machine has just woken up.
    pub fn woke(&mut self, now: Instant) {
        self.start_over();
        self.woke = Some(now);
        self.button_down = None;
    }

    /// Whether the controller should be passed down to the compositor now.
    pub fn controller_used(&mut self, now: Instant) -> bool {
        let due = self
            .last_activity
            .is_none_or(|last| now.saturating_duration_since(last) >= ACTIVITY_EVERY);
        if due {
            self.last_activity = Some(now);
        }
        due
    }

    /// What the screens are set to now.
    pub fn applied(&self) -> Screens {
        self.applied
    }

    /// The power button went down or came up. Returns what a *press* does,
    /// which is decided on the way up; a hold is answered by [`Self::pass`].
    pub fn power_button(&mut self, pressed: bool, now: Instant) -> bool {
        if self
            .woke
            .is_some_and(|woke| now.saturating_duration_since(woke) < AFTER_WAKING)
        {
            tracing::info!("the power button, just after waking: taken as the press that woke");
            self.button_down = None;
            return false;
        }
        if pressed {
            self.button_down = Some(now);
            self.menu_opened = false;
            return false;
        }
        let pressed_at = self.button_down.take();
        pressed_at.is_some() && !self.menu_opened
    }

    /// Work out what is to be done this pass.
    #[allow(clippy::too_many_arguments)]
    pub fn pass(
        &mut self,
        now: Instant,
        holds: Holds,
        battery: Option<(u8, bool)>,
        owns_the_machine: bool,
    ) -> Doing {
        let mut doing = Doing::default();

        let wanted = screens(self.dim_idle, self.off_idle);
        if wanted != self.applied {
            self.applied = wanted;
            self.applied_at = Some(now);
            doing.screens = Some(wanted);
        }

        // Held long enough for the menu, and not answered yet.
        if !self.menu_opened
            && self
                .button_down
                .is_some_and(|down| now.saturating_duration_since(down) >= HOLD_FOR_THE_MENU)
        {
            self.menu_opened = true;
            doing.power_menu = true;
        }

        // The battery. Warnings first, then — for one that is about to run
        // out and has been told so — sleep, whatever holds it, because the
        // alternative is the machine dying with everything open.
        match battery {
            Some((percent, on_battery)) => {
                if !on_battery {
                    self.warned = None;
                    self.empty_since = None;
                }
                if let Some(step) = battery_step(percent, on_battery, self.warned) {
                    self.warned = Some(step);
                    doing.warn = Some((step, percent));
                    if step == BatteryStep::Empty {
                        self.empty_since = Some(now);
                    }
                }
            }
            None => {
                self.warned = None;
                self.empty_since = None;
            }
        }
        let emptying = self
            .empty_since
            .is_some_and(|since| now.saturating_duration_since(since) >= EMPTY_GRACE);

        let retry_due = self.sleep_asked.is_none_or(|asked| {
            now.saturating_duration_since(asked) >= crate::power_bus::RETRY_SLEEP
        });
        if holds.any() != self.held {
            self.held = holds.any();
            self.said_why = false;
        }
        if (self.sleep_idle || emptying) && retry_due {
            // What holds sleep is asked before whose machine it is, so a nested
            // session's log says what a real one would have done.
            if holds.any() && !emptying {
                if !self.said_why {
                    tracing::info!(?holds, "the machine is idle, but something holds sleep off");
                    self.said_why = true;
                }
            } else if !owns_the_machine {
                if !self.said_why {
                    tracing::info!(
                        "the machine would sleep now, but this session is not on its own displays"
                    );
                    self.said_why = true;
                }
            } else {
                self.sleep_asked = Some(now);
                self.empty_since = None;
                doing.sleep = true;
            }
        }
        doing
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEFAULT: PowerSettings = PowerSettings::DEFAULT;

    #[test]
    fn the_waits_follow_the_settings_and_the_power_source() {
        let on_mains = waits(DEFAULT, false);
        assert_eq!(on_mains.dim, Some(120_000));
        assert_eq!(on_mains.off, Some(300_000));
        assert_eq!(on_mains.sleep, Some(3_600_000));
        assert_eq!(waits(DEFAULT, true).sleep, Some(900_000));

        let never = PowerSettings {
            dim_after: 0,
            screen_off_after: 0,
            sleep_on_battery: 0,
            sleep_plugged_in: 0,
            ..DEFAULT
        };
        assert_eq!(waits(never, true), Waits::default());
    }

    /// A dim that would come at or after the screen going dark is left out,
    /// and a dim with no screen-off at all is kept.
    #[test]
    fn dimming_only_comes_before_the_dark() {
        let late = PowerSettings {
            dim_after: 600,
            screen_off_after: 300,
            ..DEFAULT
        };
        assert_eq!(waits(late, false).dim, None);
        let same = PowerSettings {
            dim_after: 300,
            screen_off_after: 300,
            ..DEFAULT
        };
        assert_eq!(waits(same, false).dim, None);
        let only = PowerSettings {
            dim_after: 60,
            screen_off_after: 0,
            ..DEFAULT
        };
        assert_eq!(waits(only, false).dim, Some(60_000));
    }

    #[test]
    fn the_screens_go_dim_then_dark() {
        assert_eq!(screens(false, false), Screens::Awake);
        assert_eq!(screens(true, false), Screens::Dim);
        assert_eq!(screens(true, true), Screens::Off);
        // The screen can go dark with no dim before it.
        assert_eq!(screens(false, true), Screens::Off);
    }

    #[test]
    fn each_battery_warning_is_given_once_on_the_way_down() {
        assert_eq!(battery_step(50, true, None), None);
        assert_eq!(battery_step(10, true, None), Some(BatteryStep::Low));
        assert_eq!(battery_step(9, true, Some(BatteryStep::Low)), None);
        assert_eq!(
            battery_step(5, true, Some(BatteryStep::Low)),
            Some(BatteryStep::VeryLow)
        );
        assert_eq!(
            battery_step(2, true, Some(BatteryStep::VeryLow)),
            Some(BatteryStep::Empty)
        );
        // A battery that was already low when it was first read is told the
        // worst of it once, not every step on the way down to it.
        assert_eq!(battery_step(4, true, None), Some(BatteryStep::VeryLow));
        // And nothing at all on the mains.
        assert_eq!(battery_step(2, false, None), None);
    }

    #[test]
    fn the_saver_comes_on_low_and_only_on_the_battery() {
        assert!(saver_wanted(true, 20, true));
        assert!(!saver_wanted(true, 21, true));
        assert!(!saver_wanted(true, 5, false));
        assert!(!saver_wanted(false, 5, true));
    }

    fn idle_for_sleep() -> Idle {
        let mut idle = Idle::default();
        idle.heard(Timer::Sleep, true);
        idle
    }

    #[test]
    fn an_idle_machine_sleeps_unless_something_holds_it() {
        let now = Instant::now();
        let mut idle = idle_for_sleep();
        assert!(idle.pass(now, Holds::default(), None, true).sleep);

        let mut held = idle_for_sleep();
        let playing = Holds {
            playing: true,
            ..Holds::default()
        };
        assert!(!held.pass(now, playing, None, true).sleep);
        // And when the music stops, still idle, it sleeps then.
        assert!(held.pass(now, Holds::default(), None, true).sleep);
    }

    /// A session that is not on the machine's own displays never sleeps it.
    #[test]
    fn a_nested_session_never_sleeps_the_machine() {
        let mut idle = idle_for_sleep();
        assert!(
            !idle
                .pass(Instant::now(), Holds::default(), None, false)
                .sleep
        );
    }

    /// A refused sleep is asked again, but not every pass.
    #[test]
    fn a_refused_sleep_is_retried_after_a_while() {
        let now = Instant::now();
        let mut idle = idle_for_sleep();
        assert!(idle.pass(now, Holds::default(), None, true).sleep);
        assert!(
            !idle
                .pass(now + Duration::from_secs(1), Holds::default(), None, true)
                .sleep
        );
        let later = now + crate::power_bus::RETRY_SLEEP;
        assert!(idle.pass(later, Holds::default(), None, true).sleep);
    }

    /// Somebody coming back takes the machine out of every wait.
    #[test]
    fn coming_back_resets_the_screens() {
        let now = Instant::now();
        let mut idle = Idle::default();
        idle.heard(Timer::Dim, true);
        assert_eq!(
            idle.pass(now, Holds::default(), None, true).screens,
            Some(Screens::Dim)
        );
        idle.heard(Timer::Off, true);
        assert_eq!(
            idle.pass(now, Holds::default(), None, true).screens,
            Some(Screens::Off)
        );
        idle.heard(Timer::Dim, false);
        idle.heard(Timer::Off, false);
        assert_eq!(
            idle.pass(now, Holds::default(), None, true).screens,
            Some(Screens::Awake)
        );
    }

    /// A battery about to run out sleeps the machine after its grace, even with
    /// music playing — and not if it is plugged in first.
    #[test]
    fn an_emptying_battery_sleeps_the_machine_whatever_holds_it() {
        let now = Instant::now();
        let playing = Holds {
            playing: true,
            ..Holds::default()
        };
        let mut idle = Idle::default();
        let first = idle.pass(now, playing, Some((3, true)), true);
        assert_eq!(first.warn, Some((BatteryStep::Empty, 3)));
        assert!(!first.sleep, "it is told first");
        assert!(
            idle.pass(now + EMPTY_GRACE, playing, Some((3, true)), true)
                .sleep
        );

        let mut plugged = Idle::default();
        plugged.pass(now, playing, Some((3, true)), true);
        assert!(
            !plugged
                .pass(now + EMPTY_GRACE, playing, Some((3, false)), true)
                .sleep
        );
    }

    #[test]
    fn a_press_is_answered_on_release_and_a_hold_opens_the_menu() {
        let now = Instant::now();
        let mut idle = Idle::default();
        assert!(!idle.power_button(true, now));
        assert!(idle.power_button(false, now + Duration::from_millis(200)));

        assert!(!idle.power_button(true, now));
        let held = idle.pass(now + HOLD_FOR_THE_MENU, Holds::default(), None, true);
        assert!(held.power_menu);
        assert!(
            !idle.power_button(false, now + HOLD_FOR_THE_MENU * 2),
            "the release after a hold is not also a press"
        );
    }

    #[test]
    fn the_press_that_woke_the_machine_is_not_a_press() {
        let now = Instant::now();
        let mut idle = Idle::default();
        idle.woke(now);
        assert!(!idle.power_button(true, now + Duration::from_millis(300)));
        assert!(!idle.power_button(false, now + Duration::from_millis(400)));
        // And a press well after waking is one.
        let later = now + AFTER_WAKING + Duration::from_secs(1);
        assert!(!idle.power_button(true, later));
        assert!(idle.power_button(false, later + Duration::from_millis(100)));
    }

    #[test]
    fn the_controller_is_passed_down_at_most_twice_a_second() {
        let now = Instant::now();
        let mut idle = Idle::default();
        assert!(idle.controller_used(now));
        assert!(!idle.controller_used(now + Duration::from_millis(100)));
        assert!(idle.controller_used(now + ACTIVITY_EVERY));
    }
}
