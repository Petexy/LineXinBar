//! Whether anybody is using the machine, and what the screens do about it.
//!
//! A handheld spends most of its battery on two things: the panel, and whatever
//! the processor and the graphics are doing while nobody is looking at either.
//! A console answers both the same way — it notices nobody has touched it for a
//! while, dims, goes dark, and eventually sleeps — and so does this session.
//! The *decisions* are the shell's, up in Settings > Power, because only the
//! shell knows about the controller and only the shell has the settings; what
//! is here is everything those decisions need from the one process that sees
//! the seat and owns the connectors:
//!
//! * **ext-idle-notify-v1**, the standard way a program asks "tell me when
//!   nobody has done anything for N seconds". Every keyboard, pointer and touch
//!   event counts, and so does `lxb_shell_v1.user_active`, which is how a
//!   controller the compositor never sees counts too. The shell makes its
//!   timers here, and so can swayidle or a screen locker.
//! * **idle-inhibit-v1**, which is how a Wayland film player says "not while I
//!   am showing this". An inhibitor holds the timers only while its surface can
//!   be seen — [`crate::render::windows_on_screen`], the same answer the frame
//!   throttle and [`crate::sleep`] use — so a film paused behind the start
//!   screen does not keep the machine awake all night.
//! * **The shell's own hold**, `lxb_shell_v1.hold_the_screen`, for the
//!   programs that ask over the session bus instead. Same effect.
//! * **Display power**, `lxb_shell_v1.set_output_power`: the sheet in
//!   [`crate::blackout`] for dim and off, and the connector switched off behind
//!   a black one by the backend.
//! * **The power button**, which is taken off the keyboard and handed to the
//!   shell — see `lxb_shell_v1.power_button`.
//!
//! Timers made with `get_input_idle_notification` ignore every hold, which is
//! the protocol's own distinction. The shell makes all three of its timers the
//! other way, so a game in front that asked for the screen keeps the machine
//! awake as well; what it does not hold is a sleep somebody asks for, which
//! never goes through a timer at all.

use std::time::{Duration, Instant};

use smithay::desktop::{layer_map_for_output, WindowSurfaceType};
use smithay::output::Output;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::Resource;
use smithay::wayland::compositor::get_parent;
use smithay::wayland::seat::WaylandFocus;

use crate::blackout::Power;
use crate::state::LxbState;

/// How often activity is passed on to the idle timers.
///
/// A mouse reports a thousand times a second and a timer that is a minute long
/// does not need to hear about every one of them: each report cancels and
/// re-arms a timer per notification. A quarter of a second is nothing against
/// the shortest wait anybody can choose, and the first movement after an idle
/// always passes, because it comes long after the last one that did — which is
/// what makes "resumed" as prompt as the input that caused it.
const NOTICE_EVERY: Duration = Duration::from_millis(250);

/// The evdev code of the power button, as the seat's keyboard reports it: xkb
/// numbers every key eight above the kernel.
pub const POWER_KEYCODE: u32 = 116 + 8;

/// What this compositor keeps about idleness beyond the timers themselves,
/// which [`smithay::wayland::idle_notify::IdleNotifierState`] keeps.
#[derive(Debug, Default)]
pub struct Idleness {
    /// Surfaces that have asked for the screen to stay on. Held whether or not
    /// they can be seen; whether they count is worked out each pass.
    inhibitors: Vec<WlSurface>,
    /// Whether the shell is holding the screen for a program that asked it over
    /// the session bus.
    held_by_shell: bool,
    /// Whether the timers are being held, as last applied — kept only so the
    /// log says when that changes rather than once a pass.
    inhibited: bool,
    /// When activity was last passed on to the timers. See [`NOTICE_EVERY`].
    last_notice: Option<Instant>,
}

impl Idleness {
    /// A surface has asked for the screen to stay on.
    pub fn inhibit(&mut self, surface: WlSurface) {
        if !self.inhibitors.contains(&surface) {
            self.inhibitors.push(surface);
        }
    }

    /// And has stopped asking.
    pub fn uninhibit(&mut self, surface: &WlSurface) {
        self.inhibitors.retain(|held| held != surface);
    }
}

impl LxbState {
    /// Somebody did something: a key, the pointer, a finger, or a controller
    /// the shell has told us about. Every idle timer starts its wait again.
    pub(crate) fn note_activity(&mut self) {
        let now = Instant::now();
        let recent = self
            .lxb
            .idleness
            .last_notice
            .is_some_and(|last| now.saturating_duration_since(last) < NOTICE_EVERY);
        if recent {
            return;
        }
        self.lxb.idleness.last_notice = Some(now);
        let seat = self.lxb.seat.clone();
        self.lxb.idle_notifier.notify_activity(&seat);
    }

    /// The shell is holding the screen for a program that asked it to, or has
    /// let go.
    pub(crate) fn hold_the_screen(&mut self, hold: bool) {
        if self.lxb.idleness.held_by_shell != hold {
            tracing::info!(hold, "the shell holds the screen for a program that asked");
        }
        self.lxb.idleness.held_by_shell = hold;
        self.refresh_idle_inhibition();
    }

    /// Work out whether anything holding the screen can be seen, and hold or
    /// release the idle timers to match.
    ///
    /// Asked once a pass of the session's loop, beside the sleep pass, and for
    /// its reason: what can be seen changes by routes that announce nothing.
    /// Cheap: a surface or two, walked to its root and looked for among the
    /// windows already listed for the frame throttle.
    pub(crate) fn refresh_idle_inhibition(&mut self) {
        // A client that went away without destroying its inhibitor leaves a
        // dead surface behind, which inhibits nothing.
        self.lxb
            .idleness
            .inhibitors
            .retain(|surface| surface.is_alive());

        let seen = !self.lxb.idleness.inhibitors.is_empty() && self.an_inhibitor_can_be_seen();
        let inhibited = seen || self.lxb.idleness.held_by_shell;
        if inhibited != self.lxb.idleness.inhibited {
            tracing::info!(
                inhibited,
                by_a_window = seen,
                by_the_shell = self.lxb.idleness.held_by_shell,
                "something is keeping the screen on"
            );
            self.lxb.idleness.inhibited = inhibited;
        }
        self.lxb.idle_notifier.set_is_inhibited(inhibited);
    }

    /// Whether any surface asking for the screen belongs to a window that is
    /// on screen, or to a layer surface on a display.
    fn an_inhibitor_can_be_seen(&self) -> bool {
        let roots: Vec<WlSurface> = self.lxb.idleness.inhibitors.iter().map(root_of).collect();
        let outputs: Vec<Output> = self.lxb.space.outputs().cloned().collect();
        outputs.iter().any(|output| {
            let windows = crate::render::windows_on_screen(&self.lxb, output);
            let in_a_window = windows.iter().any(|window| {
                window
                    .wl_surface()
                    .is_some_and(|surface| roots.contains(&*surface))
            });
            in_a_window || {
                let layers = layer_map_for_output(output);
                roots.iter().any(|root| {
                    layers
                        .layer_for_surface(root, WindowSurfaceType::ALL)
                        .is_some()
                })
            }
        })
    }

    /// Dim a display, switch it off, or bring it back — the shell's word, which
    /// is the only word about it. See `lxb_shell_v1.set_output_power`.
    pub fn set_output_power(&mut self, output: &Output, power: Power) {
        let was = self.lxb.blackouts.power(output);
        if was == power {
            return;
        }
        tracing::info!(display = %output.name(), ?was, now = ?power, "display power");
        self.lxb.blackouts.set_power(output, power, Instant::now());
        // Continued here, with the request that brings it back, rather than on
        // the next pass — for the reason `cover_output_in_black` gives.
        self.refresh_application_sleep();
        self.queue_redraw();
        // And once more when the sheet has finished coming down, because that
        // is the moment the backend switches the connector off, and a display
        // showing a still black frame has no other reason to be drawn again.
        if power == Power::Off {
            let after = crate::blackout::DOWN + Duration::from_millis(50);
            let res = self.lxb.loop_handle.insert_source(
                smithay::reexports::calloop::timer::Timer::from_duration(after),
                |_, _, state| {
                    state.queue_redraw();
                    smithay::reexports::calloop::timer::TimeoutAction::Drop
                },
            );
            if let Err(err) = res {
                tracing::warn!(?err, "could not schedule switching a display off");
            }
        }
    }

    /// Bring every display back on: the shell that asked for them to be dimmed
    /// or switched off has gone, and nobody else will ever ask for them back.
    pub(crate) fn every_display_on(&mut self) {
        if !self.lxb.blackouts.any_powered_down() {
            return;
        }
        tracing::info!("bringing every display back on");
        let outputs: Vec<Output> = self.lxb.space.outputs().cloned().collect();
        for output in outputs {
            self.set_output_power(&output, Power::On);
        }
    }
}

/// The surface at the top of `surface`'s tree, which is what a window or a
/// layer surface is known by.
fn root_of(surface: &WlSurface) -> WlSurface {
    let mut root = surface.clone();
    while let Some(parent) = get_parent(&root) {
        root = parent;
    }
    root
}
