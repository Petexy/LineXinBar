//! Input routing and compositor keybindings.

use smithay::backend::input::{
    AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event, InputBackend, InputEvent,
    KeyState, KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent, PointerMotionEvent,
    TouchEvent,
};
use smithay::desktop::{layer_map_for_output, Window, WindowSurfaceType};
use smithay::input::keyboard::{keysyms, xkb, FilterResult, Keysym, ModifiersState};
use smithay::input::pointer::{AxisFrame, ButtonEvent, MotionEvent, RelativeMotionEvent};
use smithay::input::touch::{DownEvent, MotionEvent as TouchMotionEvent, UpEvent};
use smithay::output::Output;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Physical, Point, Size, SERIAL_COUNTER};
use smithay::wayland::compositor::RegionAttributes;
use smithay::wayland::pointer_constraints::{with_pointer_constraint, PointerConstraint};
use smithay::wayland::seat::WaylandFocus;
use smithay::wayland::shell::wlr_layer::Layer as WlrLayer;
use smithay::xwayland::xwm::WmWindowType;

use crate::config::Config;
use crate::focus::{x11_surface_matches, KeyboardFocusTarget};
use crate::state::LxbState;
use crate::xwayland::x11_window_accepts_input;

#[derive(Clone)]
enum ActivePointerConstraint {
    Locked,
    Confined(Option<RegionAttributes>),
}

/// A compositor level action, triggered by a keybinding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Shut the compositor down.
    Quit,
    /// Ask the focused toplevel to close.
    CloseWindow,
    /// Run a command.
    Spawn(String),
    /// Change to a Linux virtual terminal. Ignored when nested.
    SwitchVt(i32),
    /// Move keyboard focus to the next / previous output.
    FocusNextOutput,
    FocusPrevOutput,
    /// Send the focused window to the next output.
    MoveWindowToNextOutput,
    /// Cycle focus between windows on the focused output.
    CycleWindow,
    /// Ask the session shell to show its guide overlay.
    ///
    /// This is the console "home" button. It has to be a compositor binding
    /// because a fullscreen application holds the keyboard, so the shell would
    /// never see the key itself.
    Guide,
    /// Ask the session shell to show its on-screen keyboard.
    ///
    /// A compositor binding for the same reason, and rather more pointedly:
    /// the application the keys are meant for is the one holding them.
    Keyboard,
    /// Photograph the display the user is on.
    ///
    /// A compositor binding for the third time, and the plainest case of all:
    /// the picture is of whatever is in front, so the key has to work while
    /// something is in front of everything.
    Screenshot,
}

impl Action {
    /// Parse the right-hand side of a keybinding entry.
    fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        if let Some(cmd) = raw.strip_prefix("spawn:") {
            return Some(Action::Spawn(cmd.trim().to_string()));
        }
        if let Some(vt) = raw.strip_prefix("vt:") {
            return vt.trim().parse().ok().map(Action::SwitchVt);
        }
        Some(match raw.to_ascii_lowercase().as_str() {
            "quit" | "exit" => Action::Quit,
            "close" | "close-window" => Action::CloseWindow,
            "focus-next-output" => Action::FocusNextOutput,
            "focus-prev-output" => Action::FocusPrevOutput,
            "move-to-next-output" => Action::MoveWindowToNextOutput,
            "cycle-window" => Action::CycleWindow,
            "guide" | "overlay" => Action::Guide,
            "keyboard" | "osk" => Action::Keyboard,
            "screenshot" | "capture-screen" => Action::Screenshot,
            _ => return None,
        })
    }
}

/// A modifier mask plus a keysym.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyPattern {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub logo: bool,
    /// Whether the modifiers are part of this chord at all.
    ///
    /// Set by writing `Any+` in front of the key, and meant for the handful of
    /// keys that are not a letter with something held down but a key with one
    /// job printed on it. The screenshot key is the case it exists for: every
    /// desktop spells a variant of it with a modifier — `Shift+Print` for the
    /// whole screen here, `Ctrl+Print` to the clipboard there, `Meta+Shift+Print`
    /// for a region — and this shell draws none of those distinctions, so all
    /// of them are the same picture and a user's hand is right whichever one it
    /// has learnt.
    ///
    /// Not something to reach for otherwise. A loose binding on a letter would
    /// take that letter away from every application in the session, in every
    /// chord it appears in.
    pub loose: bool,
    pub keysym: Keysym,
}

impl KeyPattern {
    /// Parse `"Super+Shift+Q"`. Modifier names are case insensitive; the final
    /// component is an xkb keysym name.
    pub fn parse(raw: &str) -> Option<Self> {
        let mut pattern = KeyPattern {
            ctrl: false,
            alt: false,
            shift: false,
            logo: false,
            loose: false,
            keysym: Keysym::from(0),
        };

        let parts: Vec<&str> = raw
            .split('+')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        let (key, mods) = parts.split_last()?;

        for m in mods {
            match m.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => pattern.ctrl = true,
                "alt" | "mod1" => pattern.alt = true,
                "shift" => pattern.shift = true,
                "super" | "logo" | "mod4" | "meta" => pattern.logo = true,
                "any" => pattern.loose = true,
                other => {
                    tracing::warn!(modifier = other, "unknown modifier in keybinding");
                    return None;
                }
            }
        }

        // xkb keysym names are case sensitive ("q" and "Q" are distinct), but
        // users write `Super+Q` meaning the physical key. Try as written, then
        // fall back to the lowercase spelling.
        let keysym = xkb::keysym_from_name(key, xkb::KEYSYM_NO_FLAGS);
        let keysym = if keysym.raw() == keysyms::KEY_NoSymbol {
            xkb::keysym_from_name(&key.to_ascii_lowercase(), xkb::KEYSYM_CASE_INSENSITIVE)
        } else {
            keysym
        };
        if keysym.raw() == keysyms::KEY_NoSymbol {
            tracing::warn!(key, "unknown keysym in keybinding");
            return None;
        }
        pattern.keysym = fold_case(keysym);
        Some(pattern)
    }

    fn matches(&self, mods: &ModifiersState, keysym: Keysym) -> bool {
        if self.keysym != fold_case(keysym) {
            return false;
        }
        self.loose
            || (self.ctrl == mods.ctrl
                && self.alt == mods.alt
                && self.logo == mods.logo
                // Shift changes the modified keysym, so compare the raw one and
                // require an exact shift match.
                && self.shift == mods.shift)
    }

    /// Whether this is about the same key as `other`, whatever is held with it.
    ///
    /// What a binding in the config file is measured against before a loose
    /// default is kept: somebody who writes down what `Print` should do has
    /// said what that key is for, and a built-in that answers for the key under
    /// every modifier would otherwise sit in front of theirs for ever.
    fn same_key(&self, other: &KeyPattern) -> bool {
        self.keysym == other.keysym
    }
}

/// Reduce a letter keysym to the key that was physically pressed.
///
/// `Super+Q` names the Q key, but pressing it without shift produces the
/// keysym `q`, and `keysym_from_name("Q")` resolves to the distinct keysym `Q`.
/// Comparing those directly means no letter binding ever matches. Folding both
/// sides leaves shift itself to the explicit modifier check, so `Super+Q` and
/// `Super+Shift+Q` stay separate bindings.
fn fold_case(keysym: Keysym) -> Keysym {
    const A: u32 = b'A' as u32;
    const Z: u32 = b'Z' as u32;

    let raw = keysym.raw();
    if (A..=Z).contains(&raw) {
        // ASCII keysyms are their character codes, so case differs by 0x20.
        return Keysym::from(raw + 0x20);
    }
    keysym
}

/// The chords that summon the guide, before anything in the config is read.
///
/// The Windows key *on its own* is the home button on a keyboard, and it is not
/// in here: a bare modifier cannot be a chord. See [`HomeTap`] for what it is
/// instead. What is left is a spelling for a keyboard whose Super key is being
/// used for something else, and the media key handhelds and remotes send for
/// exactly this.
const GUIDE_BINDINGS: [&str; 2] = ["Super+Home", "XF86HomePage"];

/// Whether a key is the Windows key, under either of its two names.
fn is_logo_key(syms: &[Keysym]) -> bool {
    syms.iter()
        .any(|sym| matches!(sym.raw(), keysyms::KEY_Super_L | keysyms::KEY_Super_R))
}

/// The Windows key on its own: the home button, on a keyboard that has none.
///
/// A modifier cannot be looked up in the binding table like any other key.
/// Held down it is half of `Super+Q` and half of every other chord in the
/// table, and pressing it is how the user *begins* one of those — so a guide
/// that opened on the press would open on the way to closing a window, and
/// every chord in the session would be shadowed by it. What names the key on
/// its own is the release: down, up, and nothing in between.
///
/// Both edges still reach the client. Swallowing the release of a modifier
/// whose press was forwarded leaves the application holding a Super that is
/// never let go of — a stuck modifier is a worse fault than an application
/// seeing a key that also meant something to the shell.
#[derive(Debug, Default)]
pub struct HomeTap {
    /// Whether a Super that is down has, so far, been pressed on its own.
    armed: bool,
    /// Whether the last event completed a tap, waiting to be acted on once the
    /// keyboard has finished with the event that produced it.
    fired: bool,
}

impl HomeTap {
    /// Note one key of the seat's keyboard.
    fn key(&mut self, logo: bool, pressed: bool) {
        self.fired = false;
        match (logo, pressed) {
            (true, true) => self.armed = true,
            (true, false) => self.fired = std::mem::take(&mut self.armed),
            // Anything else going down while Super is held makes a chord of it,
            // and a chord is not a tap however it ends.
            (false, true) => self.armed = false,
            (false, false) => {}
        }
    }

    /// Whether the guide is owed a tap, clearing the debt.
    fn take(&mut self) -> bool {
        std::mem::take(&mut self.fired)
    }

    /// Whatever was under way, it was not a tap.
    ///
    /// A hand that has reached the mouse is a hand that has left the chord it
    /// was in the middle of; so is a session that has just had the keys taken
    /// off it, which would otherwise hold the arming across the gap and open
    /// the guide on a release belonging to somewhere else entirely.
    pub fn interrupt(&mut self) {
        self.armed = false;
        self.fired = false;
    }
}

/// The button on the side of a mouse, as Linux numbers it.
///
/// The rear one — `BTN_SIDE`, what a browser reads as Back — because that is
/// what the guide is: the way back out of whatever is in front of it. Its
/// neighbour is deliberately left alone. A mouse has two of these, and taking
/// both would leave a browser running inside the session with no way forward.
const BTN_SIDE: u32 = 0x113;

/// The compositor's keybinding table.
#[derive(Debug, Default)]
pub struct KeyBindings {
    /// The chords that summon the guide, kept apart from the rest and looked
    /// up first.
    ///
    /// The guide is the way *out* of whatever is running: a fullscreen
    /// application holds the keyboard, and this binding is the only thing
    /// standing between the user and a session they cannot get back. So it
    /// outranks every other binding, and nothing in the config can take one of
    /// these chords over for something else — a home button that the
    /// configuration file can accidentally remove is not a home button.
    guide: Vec<KeyPattern>,
    bindings: Vec<(KeyPattern, Action)>,
}

impl KeyBindings {
    pub fn from_config(config: &Config) -> Self {
        let mut guide: Vec<KeyPattern> = GUIDE_BINDINGS
            .iter()
            .filter_map(|raw| KeyPattern::parse(raw))
            .collect();
        let mut bindings: Vec<(KeyPattern, Action)> = Vec::new();

        // Built-in defaults. Explicit config entries override these.
        let defaults = [
            ("Ctrl+Alt+BackSpace", Action::Quit),
            ("Super+Q", Action::CloseWindow),
            // The on-screen keyboard, twice over: a chord for keyboards, and
            // the media key a handheld or a remote sends for exactly this.
            ("Super+K", Action::Keyboard),
            ("XF86Keyboard", Action::Keyboard),
            // The screenshot key, three times over, because it is the one
            // control here that people arrive already knowing a spelling for
            // and no two of them know the same one.
            //
            // `Any+Print` is the key with a picture of it printed on the
            // keyboard, and it is deliberately deaf to the modifiers: every
            // desktop puts a different variant of the picture on each of them —
            // the whole screen on `Shift+Print`, the clipboard on `Ctrl+Print`,
            // a region on `Meta+Shift+Print` — and this shell takes one kind of
            // picture, so a hand that learnt any of those is right.
            //
            // The other two are the chord every Mac has had for thirty years,
            // in both of the ways it gets transcribed onto a PC keyboard:
            // Command read as Control, and Command read as the key that sits
            // where it sits, which is Alt.
            ("Any+Print", Action::Screenshot),
            ("Ctrl+Shift+3", Action::Screenshot),
            ("Alt+Shift+3", Action::Screenshot),
            ("Super+Tab", Action::CycleWindow),
            ("Super+Right", Action::FocusNextOutput),
            ("Super+Left", Action::FocusPrevOutput),
            ("Super+Shift+Right", Action::MoveWindowToNextOutput),
        ];

        for (raw, action) in defaults {
            if config.keybindings.contains_key(raw) {
                continue;
            }
            if let Some(pattern) = KeyPattern::parse(raw) {
                bindings.push((pattern, action));
            }
        }

        // Ctrl+Alt+F1..F12 switch VTs, matching every other Linux compositor.
        for vt in 1..=12 {
            if let Some(pattern) = KeyPattern::parse(&format!("Ctrl+Alt+F{vt}")) {
                bindings.push((pattern, Action::SwitchVt(vt)));
            }
        }

        for (raw, action) in &config.keybindings {
            match (KeyPattern::parse(raw), Action::parse(action)) {
                (Some(pattern), Some(Action::Guide)) => {
                    // A chord the user *added* for the guide joins the ones
                    // that cannot be taken away, and stops being whatever else
                    // it was bound to.
                    bindings.retain(|(p, _)| *p != pattern);
                    if !guide.contains(&pattern) {
                        guide.push(pattern);
                    }
                }
                (Some(pattern), Some(action)) if guide.contains(&pattern) => {
                    tracing::warn!(
                        binding = raw,
                        ?action,
                        "ignoring a keybinding that would take over the guide"
                    );
                }
                (Some(pattern), Some(action)) => {
                    // The chord itself, and any built-in that answers for the
                    // whole key: a user who has said what `Print` does has said
                    // it about the key, and a loose default standing in front of
                    // theirs would make the config file look ignored.
                    bindings.retain(|(p, _)| *p != pattern && !(p.loose && p.same_key(&pattern)));
                    bindings.push((pattern, action));
                }
                (None, _) => tracing::warn!(binding = raw, "ignoring unparseable keybinding"),
                (_, None) => tracing::warn!(action = action, "ignoring unknown action"),
            }
        }

        Self { guide, bindings }
    }

    /// The action one keypress runs, if any.
    ///
    /// `syms` is every spelling of the key that was pressed — the modified
    /// symbol and the raw ones — because a chord is written the way the user
    /// thinks of the key, not the way the layout renders it under the
    /// modifiers being held.
    fn lookup<I>(&self, mods: &ModifiersState, syms: I) -> Option<Action>
    where
        I: IntoIterator<Item = Keysym>,
        I::IntoIter: Clone,
    {
        let mut syms = syms.into_iter();
        // Every spelling is tried against the guide before any of them is
        // tried against anything else, so a key that happens to resolve to
        // another binding under one of its symbols cannot get in ahead of the
        // way out of a fullscreen application.
        if syms
            .clone()
            .any(|sym| self.guide.iter().any(|pattern| pattern.matches(mods, sym)))
        {
            return Some(Action::Guide);
        }
        syms.find_map(|sym| {
            self.bindings
                .iter()
                .find(|(pattern, _)| pattern.matches(mods, sym))
        })
        .map(|(_, action)| action.clone())
    }
}

impl LxbState {
    /// Feed one backend input event into the compositor.
    pub fn process_input_event<B: InputBackend>(&mut self, event: InputEvent<B>) {
        self.process_input_event_mapped(event, None, None);
    }

    /// Feed an input event that originated from a particular nested output.
    ///
    /// The X11 backend has one host window per output. Absolute events carry
    /// coordinates relative to that host window, so losing the window/output
    /// association would route every event to whichever output happened to
    /// contain the pointer previously.
    pub fn process_input_event_for_output<B: InputBackend>(
        &mut self,
        event: InputEvent<B>,
        output: Option<Output>,
    ) {
        self.process_input_event_mapped(event, output, None);
    }

    /// Feed an event from a nested host window whose physical source size is
    /// known. Using the raw coordinates avoids relying on backend-normalised
    /// touch coordinates (Smithay 0.7's winit adapter normalises touch Y by
    /// the window width).
    pub fn process_input_event_from_window<B: InputBackend>(
        &mut self,
        event: InputEvent<B>,
        output: Output,
        source_size: Size<i32, Physical>,
    ) {
        self.process_input_event_mapped(event, Some(output), Some(source_size));
    }

    fn process_input_event_mapped<B: InputBackend>(
        &mut self,
        event: InputEvent<B>,
        output: Option<Output>,
        source_size: Option<Size<i32, Physical>>,
    ) {
        match event {
            InputEvent::Keyboard { event } => self.on_keyboard::<B>(event),
            InputEvent::PointerMotion { event } => self.on_pointer_motion::<B>(event),
            InputEvent::PointerMotionAbsolute { event } => {
                self.on_pointer_motion_absolute::<B>(event, output.as_ref(), source_size)
            }
            InputEvent::PointerButton { event } => self.on_pointer_button::<B>(event),
            InputEvent::PointerAxis { event } => self.on_pointer_axis::<B>(event),
            InputEvent::TouchDown { event } => {
                self.on_touch_down::<B>(event, output.as_ref(), source_size)
            }
            InputEvent::TouchMotion { event } => {
                self.on_touch_motion::<B>(event, output.as_ref(), source_size)
            }
            InputEvent::TouchUp { event } => self.on_touch_up::<B>(event),
            InputEvent::TouchCancel { .. } => {
                if let Some(touch) = self.lxb.seat.get_touch() {
                    touch.cancel(self);
                }
            }
            InputEvent::TouchFrame { .. } => {
                self.touch_frame();
            }
            _ => {}
        }
    }

    /// Finish a batch of touch events. Nested backends without explicit touch
    /// frame events call this after each event.
    pub fn touch_frame(&mut self) {
        if let Some(touch) = self.lxb.seat.get_touch() {
            touch.frame(self);
        }
    }

    fn on_keyboard<B: InputBackend>(&mut self, event: B::KeyboardKeyEvent) {
        let serial = SERIAL_COUNTER.next_serial();
        let time = Event::time_msec(&event);
        let keycode = event.key_code();
        let state = event.state();

        let Some(keyboard) = self.lxb.seat.get_keyboard() else {
            return;
        };
        let pressed = state == KeyState::Pressed;

        let action = keyboard.input(self, keycode, state, serial, time, |state, mods, handle| {
            // Which key this is, in the one place the symbols it produces are
            // known. Every key passes through here, because what makes a tap a
            // tap is as much the keys that are *not* the Windows key.
            state
                .lxb
                .home_tap
                .key(is_logo_key(&handle.raw_syms()), pressed);

            // The modified symbol as well as the raw ones, so that both
            // `Super+Q` and shift-rewritten combos like `Super+Shift+Right`
            // resolve to the same binding.
            let candidates = std::iter::once(handle.modified_sym()).chain(handle.raw_syms());
            match state.lxb.keybindings.lookup(mods, candidates) {
                Some(action) => FilterResult::Intercept(action),
                None => FilterResult::Forward,
            }
        });

        // A key was pressed on a keyboard the compositor can see, which is the
        // user telling it they are not using the mouse. Done for every key,
        // including the ones that turn out to be bindings: the guide button is
        // as much a hand off the mouse as a letter is.
        if pressed {
            self.pointer_put_down();
        }

        // Actions fire on press only; the matching release is swallowed too,
        // which is what clients expect from a grabbed binding.
        if let Some(action) = action {
            if pressed {
                self.run_action(action);
            }
        }

        // And the home button last, on the release of a Windows key that was
        // pressed on its own. After the binding table rather than before it,
        // because a tap cannot be a chord: by the time one has completed there
        // is nothing else this event could also have been.
        if self.lxb.home_tap.take() {
            self.run_action(Action::Guide);
        }
    }

    fn run_action(&mut self, action: Action) {
        tracing::debug!(?action, "running action");
        match action {
            Action::Quit => {
                self.lxb.running = false;
                self.lxb.loop_signal.stop();
            }
            Action::CloseWindow => {
                if let Some(window) = self.focused_window() {
                    self.request_window_close(&window);
                }
            }
            Action::Spawn(cmd) => self.lxb.spawn(&cmd),
            Action::SwitchVt(vt) => self.backend.switch_vt(vt),
            Action::FocusNextOutput => self.cycle_output(1),
            Action::FocusPrevOutput => self.cycle_output(-1),
            Action::MoveWindowToNextOutput => self.move_window_to_next_output(),
            Action::CycleWindow => self.cycle_window(),
            Action::Guide => self.open_guide(),
            Action::Keyboard => self.open_keyboard(),
            Action::Screenshot => self.screenshot_focused_output(),
        }
    }

    /// Ask a window to close itself, whichever shell it speaks.
    ///
    /// This is a request, not a kill: the client may prompt about unsaved work
    /// or ignore it entirely.
    pub fn request_window_close(&mut self, window: &Window) {
        if let Some(toplevel) = window.toplevel() {
            toplevel.send_close();
        } else if let Some(surface) = window.x11_surface() {
            if let Err(err) = surface.close() {
                tracing::warn!(?err, "failed to close X11 window");
            }
        }
    }

    // -- pointer ---------------------------------------------------------

    /// The pointer moved, so there is a hand on something that moves it.
    ///
    /// Movement is the only thing that brings the cursor back, and that is the
    /// whole of the rule: a cursor which reappeared because a button was
    /// pressed or a wheel turned would appear wherever it happened to have
    /// been left, which is nowhere the user is looking. Moving it is the one
    /// gesture that also says *where*.
    ///
    /// Every source counts, because every source is somebody pointing:
    /// a mouse, a touchpad, XWayland's own pointer, and the shell's stick —
    /// which exists precisely to be a mouse where there is none.
    fn pointer_moved(&mut self) {
        self.set_pointer_visible(true);
    }

    /// The user reached for something that is not the pointer.
    ///
    /// A key on a real keyboard, or — over `lxb_shell_v1.hide_pointer`, since
    /// a controller is not a seat device — a button on a gamepad. Either way
    /// the mouse has been let go of, and the arrow left sitting on the screen
    /// is in the way of what the user is actually doing.
    pub fn pointer_put_down(&mut self) {
        self.set_pointer_visible(false);
    }

    fn set_pointer_visible(&mut self, visible: bool) {
        if self.lxb.pointer_visible == visible {
            return;
        }
        self.lxb.pointer_visible = visible;
        // Logged because an absent cursor is otherwise indistinguishable from
        // a broken one, and this is the only thing that can tell them apart.
        tracing::debug!(visible, "cursor");
        // The cursor appearing or vanishing is a change to the screen that
        // nothing else is going to report: on a still desktop there is no
        // other damage to ride along with.
        self.queue_redraw();
    }

    fn on_pointer_motion<B: InputBackend>(&mut self, event: B::PointerMotionEvent) {
        self.pointer_motion_by(
            event.delta(),
            event.delta_unaccel(),
            event.time(),
            event.time_msec(),
        );
    }

    /// One relative pointer movement, from wherever it came from.
    ///
    /// Split out of [`Self::on_pointer_motion`] because the shell's stick
    /// pointer arrives without an input event to carry it: a controller is not
    /// a seat device, so `lxb_shell_v1.move_pointer` hands over a bare
    /// delta. Everything after that has to be identical — the constraints a
    /// game holds, the relative stream it reads its camera from, the surface
    /// the motion is delivered to — because a pointer moved two different ways
    /// is two pointers as far as the application can tell.
    fn pointer_motion_by(
        &mut self,
        delta: Point<f64, Logical>,
        delta_unaccel: Point<f64, Logical>,
        utime: u64,
        time_msec: u32,
    ) {
        let serial = SERIAL_COUNTER.next_serial();
        let Some(pointer) = self.lxb.seat.get_pointer() else {
            return;
        };
        self.pointer_moved();
        self.apply_released_pointer_hint(&pointer);

        let old_location = self.lxb.pointer_location;
        let old_hit = self.surface_under(old_location);
        let old_under = old_hit
            .as_ref()
            .map(|(surface, origin)| (self.input_target_for_surface(surface), *origin));
        let constraint = old_hit.as_ref().and_then(|(surface, origin)| {
            active_pointer_constraint(&pointer, surface, old_location - *origin)
        });

        // Relative motion is delivered even while the logical pointer is
        // locked. Games use this stream for camera movement while wl_pointer
        // remains stationary.
        pointer.relative_motion(
            self,
            old_under.clone(),
            &RelativeMotionEvent {
                delta,
                delta_unaccel,
                utime,
            },
        );

        if matches!(constraint, Some(ActivePointerConstraint::Locked)) {
            pointer.frame(self);
            return;
        }

        self.lxb.pointer_location = old_location + delta;
        self.clamp_pointer();
        let location = self.lxb.pointer_location;
        let hit = self.surface_under(location);

        if !confined_motion_is_valid(
            constraint.as_ref(),
            old_hit.as_ref(),
            hit.as_ref(),
            location,
        ) {
            self.lxb.pointer_location = old_location;
            pointer.frame(self);
            return;
        }

        let under = hit
            .as_ref()
            .map(|(surface, origin)| (self.input_target_for_surface(surface), *origin));

        pointer.motion(
            self,
            under.clone(),
            &MotionEvent {
                location,
                serial,
                time: time_msec,
            },
        );
        self.activate_constraint_at(&pointer, hit.as_ref(), location);
        pointer.frame(self);
    }

    fn on_pointer_motion_absolute<B: InputBackend>(
        &mut self,
        event: B::PointerMotionAbsoluteEvent,
        target_output: Option<&Output>,
        source_size: Option<Size<i32, Physical>>,
    ) {
        let serial = SERIAL_COUNTER.next_serial();
        self.pointer_moved();

        // Absolute devices report in 0.0..=1.0 of one output's area.
        let output = target_output.cloned().or_else(|| {
            self.lxb
                .outputs
                .output_at(&self.lxb.space, self.lxb.pointer_location)
                .or_else(|| self.lxb.space.outputs().next().cloned())
        });
        let Some(output) = output else { return };
        let Some(geometry) = self.lxb.space.output_geometry(&output) else {
            return;
        };

        let Some(pointer) = self.lxb.seat.get_pointer() else {
            return;
        };
        self.apply_released_pointer_hint(&pointer);

        let reported_location =
            geometry.loc.to_f64() + absolute_position::<B, _>(&event, geometry.size, source_size);
        let nested = !matches!(self.backend, crate::backend::Backend::Udev(_));
        let nested_delta = if nested {
            self.lxb
                .nested_host_pointer_location
                .replace(reported_location)
                .map(|previous| reported_location - previous)
        } else {
            None
        };

        let old_location = self.lxb.pointer_location;
        let old_hit = self.surface_under(old_location);
        let old_under = old_hit
            .as_ref()
            .map(|(surface, origin)| (self.input_target_for_surface(surface), *origin));
        let constraint = old_hit.as_ref().and_then(|(surface, origin)| {
            active_pointer_constraint(&pointer, surface, old_location - *origin)
        });

        // Nested backends only expose host cursor coordinates. Deriving a
        // delta here keeps wp_relative_pointer useful for nested testing; the
        // native libinput backend supplies true unaccelerated relative events.
        if let Some(delta) = nested_delta.filter(|delta| delta.x != 0.0 || delta.y != 0.0) {
            pointer.relative_motion(
                self,
                old_under,
                &RelativeMotionEvent {
                    delta,
                    delta_unaccel: delta,
                    utime: event.time(),
                },
            );
        }

        if matches!(constraint, Some(ActivePointerConstraint::Locked)) {
            pointer.frame(self);
            return;
        }

        // Keep nested logical motion delta-based after the first sample. This
        // preserves a client's cursor-position hint when a lock is released;
        // snapping straight back to the host's stale absolute coordinate
        // would otherwise erase the hint in this same event.
        self.lxb.pointer_location = if nested {
            nested_delta
                .map(|delta| old_location + delta)
                .unwrap_or(reported_location)
        } else {
            reported_location
        };
        self.clamp_pointer();
        let location = self.lxb.pointer_location;
        let hit = self.surface_under(location);
        if !confined_motion_is_valid(
            constraint.as_ref(),
            old_hit.as_ref(),
            hit.as_ref(),
            location,
        ) {
            self.lxb.pointer_location = old_location;
            pointer.frame(self);
            return;
        }

        let under = hit
            .as_ref()
            .map(|(surface, origin)| (self.input_target_for_surface(surface), *origin));
        pointer.motion(
            self,
            under,
            &MotionEvent {
                location,
                serial,
                time: event.time_msec(),
            },
        );
        self.activate_constraint_at(&pointer, hit.as_ref(), location);
        pointer.frame(self);
    }

    fn activate_constraint_at(
        &mut self,
        pointer: &smithay::input::pointer::PointerHandle<Self>,
        hit: Option<&(WlSurface, Point<f64, Logical>)>,
        location: Point<f64, Logical>,
    ) {
        let Some((surface, origin)) = hit else {
            return;
        };
        with_pointer_constraint(surface, pointer, |constraint| {
            if let Some(constraint) = constraint {
                let inside = constraint
                    .region()
                    .map(|region| region.contains((location - *origin).to_i32_round()))
                    .unwrap_or(true);
                if inside && !constraint.is_active() {
                    constraint.activate();
                }
            }
        });
    }

    /// Apply a locked-pointer cursor hint once the client has released its
    /// constraint. Until then the stored logical location must stay fixed.
    fn apply_released_pointer_hint(
        &mut self,
        pointer: &smithay::input::pointer::PointerHandle<Self>,
    ) {
        let Some((surface, hint)) = self.lxb.pointer_position_hint.clone() else {
            return;
        };
        let mut still_locked = false;
        with_pointer_constraint(&surface, pointer, |constraint| {
            still_locked = constraint.is_some_and(|constraint| constraint.is_active());
        });
        if still_locked {
            return;
        }

        let focus_matches = pointer
            .current_focus()
            .and_then(|focus| {
                focus
                    .wl_surface()
                    .map(|current| current.as_ref() == &surface)
            })
            .unwrap_or(false);
        if !focus_matches {
            self.lxb.pointer_position_hint = None;
            return;
        }

        let Some((current, origin)) = self.surface_under(self.lxb.pointer_location) else {
            self.lxb.pointer_position_hint = None;
            return;
        };
        if current != surface {
            self.lxb.pointer_position_hint = None;
            return;
        }

        self.lxb.pointer_location = origin + hint;
        self.clamp_pointer();
        self.lxb.pointer_position_hint = None;
    }

    fn on_pointer_button<B: InputBackend>(&mut self, event: B::PointerButtonEvent) {
        let button = event.button_code();
        let state = event.state();
        // A hand that has arrived at the mouse is a hand that has left whatever
        // chord it was in the middle of.
        self.lxb.home_tap.interrupt();

        // The home button, on a mouse. Held back from the client outright —
        // both edges, so nothing is left half pressed — for the same reason the
        // guide's chords are held back: it is the way out of an application
        // that is holding everything else, and an application that could take
        // it over would be an application there is no way out of.
        if button == BTN_SIDE {
            if state == ButtonState::Pressed {
                self.run_action(Action::Guide);
            }
            return;
        }

        self.pointer_button_at(button, state, event.time_msec());
    }

    /// One pointer button, from wherever it came from — see
    /// [`Self::pointer_motion_by`] for why that is a distinction worth making.
    fn pointer_button_at(&mut self, button: u32, state: ButtonState, time_msec: u32) {
        let serial = SERIAL_COUNTER.next_serial();

        if state == ButtonState::Pressed {
            self.focus_under_pointer(serial);
        }

        if let Some(pointer) = self.lxb.seat.get_pointer() {
            pointer.button(
                self,
                &ButtonEvent {
                    button,
                    state,
                    serial,
                    time: time_msec,
                },
            );
            pointer.frame(self);
        }
    }

    // -- the shell's stick pointer ---------------------------------------
    //
    // A controller is not a seat device: the shell reads it from /dev/input,
    // which is the only reason it works while a game holds the keyboard, and
    // hands the result over as `lxb_shell_v1.move_pointer`. From here on
    // it is an ordinary pointer movement, because anything else would be a
    // second pointer the application has to be taught about.

    /// Move the pointer by a relative amount on the shell's behalf.
    ///
    /// Refused while nothing is running: the stick is a mouse *inside an
    /// application*, and a shell that left it on would otherwise walk the
    /// cursor across a desktop that has only the launcher on it.
    /// Follow XWayland's pointer when something has moved it behind our back.
    ///
    /// An X client can synthesise pointer motion with XTEST, and XWayland
    /// handles that entirely inside itself: it moves its own pointer and tells
    /// the compositor nothing. Steam does this to drive the Steam Controller's
    /// trackpad once it has claimed the pad — measured, and the motion reaches
    /// no evdev device on the machine — so its own windows follow a pointer
    /// the compositor has never heard of, and the cursor is left behind
    /// wherever a real mouse last put it.
    ///
    /// The rule is not "make ours equal theirs", which would fight every real
    /// mouse movement: XWayland learns of ours a moment after it happens, so
    /// during a fast drag their pointer is always a little stale, and copying
    /// it back would drag the cursor backwards. It is "follow theirs *only*
    /// when ours did not move" — the one case where a difference can have come
    /// from nowhere but their side.
    pub fn follow_xwayland_pointer(&mut self) {
        let Some((x, y)) = self
            .lxb
            .x11_focus_probe
            .as_ref()
            .and_then(|probe| probe.pointer_position())
        else {
            return;
        };
        let theirs = Point::<f64, Logical>::from((f64::from(x), f64::from(y)));
        let ours = self.lxb.pointer_location;

        let moved_here = self.lxb.last_synced_pointer.is_none_or(|last| last != ours);
        let was = self.lxb.last_xwayland_pointer.replace(theirs);
        self.lxb.last_synced_pointer = Some(ours);

        let delta = theirs - ours;
        if !xwayland_moved_alone(moved_here, was, theirs, delta) {
            return;
        }

        let time = self.monotonic_msec();
        self.pointer_motion_by(delta, delta, u64::from(time) * 1000, time);
        self.lxb.last_synced_pointer = Some(self.lxb.pointer_location);
    }

    pub fn shell_move_pointer(&mut self, delta: Point<f64, Logical>) {
        if delta.x == 0.0 && delta.y == 0.0 {
            return;
        }
        if !self.has_application_window() {
            return;
        }
        let time = self.monotonic_msec();
        // The unaccelerated delta is the same number: a stick has no
        // acceleration curve of the compositor's to undo, and a game reading
        // the raw stream should see what the shell actually sent.
        self.pointer_motion_by(delta, delta, u64::from(time) * 1000, time);
    }

    /// Scroll under the pointer on the shell's behalf.
    ///
    /// Described to clients as a continuous source rather than as wheel
    /// notches, because that is what a stick is: pushed further to scroll
    /// faster, like a touchpad dragged further, with no detent anywhere in it
    /// to round the movement to.
    pub fn shell_scroll_pointer(&mut self, dx: f64, dy: f64) {
        if dx == 0.0 && dy == 0.0 {
            return;
        }
        if !self.has_application_window() {
            return;
        }
        let Some(pointer) = self.lxb.seat.get_pointer() else {
            return;
        };
        self.ensure_pointer_focus(&pointer);

        let mut frame = AxisFrame::new(self.monotonic_msec()).source(AxisSource::Continuous);
        if dx != 0.0 {
            frame = frame.value(Axis::Horizontal, dx);
        }
        if dy != 0.0 {
            frame = frame.value(Axis::Vertical, dy);
        }
        pointer.axis(self, frame);
        pointer.frame(self);
    }

    /// Press or release a pointer button on the shell's behalf.
    pub fn shell_pointer_button(&mut self, button: u32, pressed: bool) {
        if !self.has_application_window() {
            return;
        }
        if let Some(pointer) = self.lxb.seat.get_pointer() {
            self.ensure_pointer_focus(&pointer);
        }
        let state = if pressed {
            ButtonState::Pressed
        } else {
            ButtonState::Released
        };
        self.pointer_button_at(button, state, self.monotonic_msec());
    }

    /// Press or release a key on the seat's keyboard on the shell's behalf.
    ///
    /// The shell has a D-pad and, while the pointer is being aimed with the
    /// other stick, nothing of its own left to do with it — so it becomes the
    /// arrows, which is what a D-pad is. That cannot be done by typing: a
    /// virtual keyboard sends *symbols*, and the arrows are the keys whose
    /// meaning a client works out from the keycode.
    ///
    /// Forwarded rather than filtered. A key the shell sent must never come
    /// back as one of the compositor's own bindings — the shell would then be
    /// pressing its own guide button, which it already has a controller for.
    ///
    /// `key` is the Linux code; xkb counts from eight higher.
    pub fn shell_keyboard_key(&mut self, key: u32, pressed: bool) {
        if !self.has_application_window() {
            return;
        }
        let Some(keyboard) = self.lxb.seat.get_keyboard() else {
            return;
        };
        let state = if pressed {
            KeyState::Pressed
        } else {
            KeyState::Released
        };
        keyboard.input::<(), _>(
            self,
            (key + 8).into(),
            state,
            SERIAL_COUNTER.next_serial(),
            self.monotonic_msec(),
            |_, _, _| FilterResult::Forward,
        );
    }

    /// Make sure the pointer has entered whatever it is sitting on before
    /// something other than motion is sent through it.
    ///
    /// A button and a scroll are both delivered to wherever the pointer
    /// *currently is*, which for a mouse is never in doubt: it entered that
    /// surface by being moved onto it. The shell's pointer can be asked to
    /// scroll or click before it has been asked to move at all — the switch is
    /// turned on in the menu and the first thing the thumb does is roll the
    /// other stick — and until it has moved there is no focus for either to
    /// reach. This is the enter that a mouse would have delivered on its way
    /// across the screen.
    ///
    /// Cheap in the ordinary case: once the focus is right it does nothing.
    fn ensure_pointer_focus(&mut self, pointer: &smithay::input::pointer::PointerHandle<Self>) {
        let location = self.lxb.pointer_location;
        let hit = self.surface_under(location);
        let wanted = hit.as_ref().map(|(surface, _)| surface.clone());
        let current = pointer
            .current_focus()
            .and_then(|focus| focus.wl_surface().map(|surface| surface.into_owned()));
        if current == wanted {
            return;
        }

        let under = hit
            .as_ref()
            .map(|(surface, origin)| (self.input_target_for_surface(surface), *origin));
        let time = self.monotonic_msec();
        pointer.motion(
            self,
            under,
            &MotionEvent {
                location,
                serial: SERIAL_COUNTER.next_serial(),
                time,
            },
        );
        self.activate_constraint_at(pointer, hit.as_ref(), location);
        pointer.frame(self);
    }

    /// Whether any application window is mapped at all.
    fn has_application_window(&self) -> bool {
        self.lxb.space.elements().any(window_accepts_keyboard_focus)
    }

    /// Milliseconds since the compositor started, which is the clock every
    /// event it synthesizes is stamped with.
    fn monotonic_msec(&self) -> u32 {
        self.lxb
            .start_time
            .elapsed()
            .as_millis()
            .min(u32::MAX as u128) as u32
    }

    fn on_pointer_axis<B: InputBackend>(&mut self, event: B::PointerAxisEvent) {
        // `Super+wheel` is a zoom in a great many applications, and a hand that
        // is doing that is not tapping the Windows key.
        self.lxb.home_tap.interrupt();

        let horizontal = event
            .amount(Axis::Horizontal)
            .unwrap_or_else(|| event.amount_v120(Axis::Horizontal).unwrap_or(0.0) / 120.0 * 15.0);
        let vertical = event
            .amount(Axis::Vertical)
            .unwrap_or_else(|| event.amount_v120(Axis::Vertical).unwrap_or(0.0) / 120.0 * 15.0);

        let mut frame = AxisFrame::new(event.time_msec()).source(event.source());
        if horizontal != 0.0 {
            frame = frame.value(Axis::Horizontal, horizontal);
            if let Some(v120) = event.amount_v120(Axis::Horizontal) {
                frame = frame.v120(Axis::Horizontal, v120 as i32);
            }
        } else if event.source() == AxisSource::Finger {
            frame = frame.stop(Axis::Horizontal);
        }
        if vertical != 0.0 {
            frame = frame.value(Axis::Vertical, vertical);
            if let Some(v120) = event.amount_v120(Axis::Vertical) {
                frame = frame.v120(Axis::Vertical, v120 as i32);
            }
        } else if event.source() == AxisSource::Finger {
            frame = frame.stop(Axis::Vertical);
        }

        if let Some(pointer) = self.lxb.seat.get_pointer() {
            pointer.axis(self, frame);
            pointer.frame(self);
        }
    }

    // -- touch -----------------------------------------------------------

    fn on_touch_down<B: InputBackend>(
        &mut self,
        event: B::TouchDownEvent,
        target_output: Option<&Output>,
        source_size: Option<Size<i32, Physical>>,
    ) {
        let Some(touch) = self.lxb.seat.get_touch() else {
            return;
        };
        let Some(output) = target_output
            .cloned()
            .or_else(|| self.lxb.space.outputs().next().cloned())
        else {
            return;
        };
        let Some(geometry) = self.lxb.space.output_geometry(&output) else {
            return;
        };

        let location =
            geometry.loc.to_f64() + absolute_position::<B, _>(&event, geometry.size, source_size);
        let serial = SERIAL_COUNTER.next_serial();
        let hit = self.surface_under(location);
        let under = hit
            .as_ref()
            .map(|(surface, origin)| (self.input_target_for_surface(surface), *origin));

        touch.down(
            self,
            under.clone(),
            &DownEvent {
                slot: event.slot(),
                location,
                serial,
                time: event.time_msec(),
            },
        );

        // Touching an on-demand layer or an application window is an explicit
        // focus request, just like clicking it with a pointer. X11 chrome
        // (menus, notifications, tooltips) still receives the touch event but
        // must not steal keyboard/controller focus from the application, and
        // neither does a layer surface that asked for no keyboard at all —
        // see [`click_takes_keyboard_focus`].
        if let Some((target, _)) = under {
            if let Some(window) = self.window_for_input_target(&target) {
                if window_accepts_keyboard_focus(&window) {
                    self.raise_window(&window, true);
                    self.set_window_keyboard_focus(&window);
                }
            } else if hit.as_ref().is_some_and(|(surface, _)| {
                click_takes_keyboard_focus(self.layer_accepts_keyboard_focus(surface))
            }) {
                self.set_keyboard_target(Some(target));
            }
        }
    }

    fn on_touch_motion<B: InputBackend>(
        &mut self,
        event: B::TouchMotionEvent,
        target_output: Option<&Output>,
        source_size: Option<Size<i32, Physical>>,
    ) {
        let Some(touch) = self.lxb.seat.get_touch() else {
            return;
        };
        let Some(output) = target_output
            .cloned()
            .or_else(|| self.lxb.space.outputs().next().cloned())
        else {
            return;
        };
        let Some(geometry) = self.lxb.space.output_geometry(&output) else {
            return;
        };

        let location =
            geometry.loc.to_f64() + absolute_position::<B, _>(&event, geometry.size, source_size);
        let under = self
            .surface_under(location)
            .map(|(surface, origin)| (self.input_target_for_surface(&surface), origin));
        touch.motion(
            self,
            under,
            &TouchMotionEvent {
                slot: event.slot(),
                location,
                time: event.time_msec(),
            },
        );
    }

    fn on_touch_up<B: InputBackend>(&mut self, event: B::TouchUpEvent) {
        let Some(touch) = self.lxb.seat.get_touch() else {
            return;
        };
        let serial = SERIAL_COUNTER.next_serial();
        touch.up(
            self,
            &UpEvent {
                slot: event.slot(),
                serial,
                time: event.time_msec(),
            },
        );
    }

    // -- focus helpers ---------------------------------------------------

    /// Keep the pointer inside the union of all output geometries, so it can
    /// never wander into the void between displays.
    fn clamp_pointer(&mut self) {
        let mut location = self.lxb.pointer_location;

        // Already inside an output: nothing to do.
        if self.lxb.space.output_under(location).next().is_some() {
            return;
        }

        // Otherwise snap to the closest point of the nearest output.
        let mut best: Option<(f64, Point<f64, Logical>)> = None;
        for output in self.lxb.space.outputs() {
            let Some(geo) = self.lxb.space.output_geometry(output) else {
                continue;
            };
            let min = geo.loc.to_f64();
            let max = (geo.loc + geo.size.to_point()).to_f64();
            let clamped = Point::<f64, Logical>::from((
                location.x.clamp(min.x, (max.x - 1.0).max(min.x)),
                location.y.clamp(min.y, (max.y - 1.0).max(min.y)),
            ));
            let dist = (clamped.x - location.x).powi(2) + (clamped.y - location.y).powi(2);
            if best.map(|(d, _)| dist < d).unwrap_or(true) {
                best = Some((dist, clamped));
            }
        }
        if let Some((_, clamped)) = best {
            location = clamped;
        }
        self.lxb.pointer_location = location;
    }

    /// Resolve the surface under a logical point, honouring layer ordering:
    /// overlay > top > windows > bottom > background.
    pub fn surface_under(
        &self,
        location: Point<f64, Logical>,
    ) -> Option<(WlSurface, Point<f64, Logical>)> {
        let output = self.lxb.outputs.output_at(&self.lxb.space, location)?;
        let output_geo = self.lxb.space.output_geometry(&output)?;
        let output_loc = output_geo.loc.to_f64();
        let relative = location - output_loc;

        let layers = layer_map_for_output(&output);
        let layer_hit = |set: [WlrLayer; 2]| {
            set.into_iter().find_map(|layer| {
                let surface = layers.layer_under(layer, relative)?;
                let layer_loc = layers
                    .layer_geometry(surface)
                    .map(|g| g.loc)
                    .unwrap_or_default();
                surface
                    .surface_under(relative - layer_loc.to_f64(), WindowSurfaceType::ALL)
                    .map(|(s, p)| (s, p.to_f64() + layer_loc.to_f64() + output_loc))
            })
        };

        if let Some(hit) = layer_hit([WlrLayer::Overlay, WlrLayer::Top]) {
            return Some(hit);
        }

        if let Some((window, window_loc)) = self.lxb.space.element_under(location) {
            if let Some((s, p)) =
                window.surface_under(location - window_loc.to_f64(), WindowSurfaceType::ALL)
            {
                return Some((s, p.to_f64() + window_loc.to_f64()));
            }
        }

        layer_hit([WlrLayer::Bottom, WlrLayer::Background])
    }

    /// The window that currently owns keyboard focus.
    pub fn focused_window(&self) -> Option<Window> {
        let keyboard = self.lxb.seat.get_keyboard()?;
        let focus = keyboard.current_focus()?;
        self.window_for_input_target(&focus)
    }

    fn window_for_input_target(&self, target: &KeyboardFocusTarget) -> Option<Window> {
        match target {
            KeyboardFocusTarget::Wayland(surface) => self.lxb.window_for_surface(surface),
            KeyboardFocusTarget::X11(surface) => self
                .lxb
                .space
                .elements()
                .find(|window| {
                    window
                        .x11_surface()
                        .is_some_and(|candidate| x11_surface_matches(candidate, surface))
                })
                .cloned(),
        }
    }

    fn input_target_for_surface(&self, surface: &WlSurface) -> KeyboardFocusTarget {
        self.lxb
            .window_for_surface(surface)
            .and_then(|window| window.x11_surface().cloned())
            .map(KeyboardFocusTarget::X11)
            .unwrap_or_else(|| KeyboardFocusTarget::Wayland(surface.clone()))
    }

    /// Set keyboard focus, unless a layer surface holds an *exclusive* grab.
    ///
    /// Only `KeyboardInteractivity::Exclusive` overrides the request; an
    /// `OnDemand` shell merely becomes the fallback in
    /// [`Self::focus_topmost_window`].
    pub fn set_keyboard_focus(&mut self, surface: Option<WlSurface>) {
        self.set_keyboard_target(surface.map(KeyboardFocusTarget::Wayland));
    }

    /// Focus a mapped window while retaining its X11 identity when needed.
    pub fn set_window_keyboard_focus(&mut self, window: &Window) {
        let target = window
            .x11_surface()
            .cloned()
            .map(KeyboardFocusTarget::X11)
            .or_else(|| {
                window
                    .wl_surface()
                    .map(|surface| KeyboardFocusTarget::Wayland(surface.into_owned()))
            });
        self.set_keyboard_target(target);
    }

    fn set_keyboard_target(&mut self, target: Option<KeyboardFocusTarget>) {
        let target = if !self.lxb.keyboard_focus_enabled {
            None
        } else {
            match &self.lxb.exclusive_keyboard_focus {
                Some(exclusive) => Some(KeyboardFocusTarget::Wayland(exclusive.clone())),
                None => target,
            }
        };
        if let Some(keyboard) = self.lxb.seat.get_keyboard() {
            // Layer surfaces commonly commit every animation frame. Avoid
            // generating serials and walking the keyboard grab for an
            // unchanged focus target.
            if keyboard.current_focus() == target {
                return;
            }
            let serial = SERIAL_COUNTER.next_serial();
            keyboard.set_focus(self, target, serial);
        }
    }

    /// React to the parent compositor focusing or unfocusing a nested host
    /// window.
    ///
    /// Clearing focus on deactivation prevents a client inside LineXinBar from
    /// remaining logically active while the user is interacting with the host
    /// desktop. Releasing the internal key state also avoids stuck modifiers:
    /// winit deliberately filters the synthetic releases it receives on focus
    /// loss.
    pub fn nested_keyboard_focus_changed(&mut self, focused: bool, output: Option<&Output>) {
        self.lxb.keyboard_focus_enabled = focused;
        self.lxb.nested_host_pointer_location = None;

        if !focused {
            self.set_keyboard_focus(None);
            self.release_pressed_keys();
            if let Some(touch) = self.lxb.seat.get_touch() {
                touch.cancel(self);
            }
            return;
        }

        match output {
            Some(output) => self.focus_topmost_on_output(output),
            None => self.focus_topmost_window(),
        }
    }

    fn release_pressed_keys(&mut self) {
        // These releases are the compositor's own, not the user's fingers
        // leaving the keys, so a Windows key that was down when focus went away
        // must not summon the guide on the way back.
        self.lxb.home_tap.interrupt();

        let Some(keyboard) = self.lxb.seat.get_keyboard() else {
            return;
        };
        let pressed: Vec<_> = keyboard.pressed_keys().into_iter().collect();
        let time = self
            .lxb
            .start_time
            .elapsed()
            .as_millis()
            .min(u32::MAX as u128) as u32;

        for keycode in pressed {
            let _ = keyboard.input::<(), _>(
                self,
                keycode,
                KeyState::Released,
                SERIAL_COUNTER.next_serial(),
                time,
                |_, _, _| FilterResult::Forward,
            );
        }
    }

    /// Whether the seat's current focus still names a mapped target that is
    /// permitted to receive keyboard events.
    ///
    /// This is intentionally a validity check, not a full policy comparison:
    /// an OnDemand layer selected by a click remains focused even while a
    /// window exists above it.
    pub fn keyboard_focus_needs_refresh(&self) -> bool {
        if !self.lxb.keyboard_focus_enabled {
            return false;
        }

        let Some(keyboard) = self.lxb.seat.get_keyboard() else {
            return false;
        };
        let Some(focus) = keyboard.current_focus() else {
            return true;
        };

        if let Some(exclusive) = &self.lxb.exclusive_keyboard_focus {
            return focus.wl_surface().as_deref() != Some(exclusive);
        }
        let window_is_mapped = match &focus {
            KeyboardFocusTarget::Wayland(surface) => self.lxb.window_for_surface(surface).is_some(),
            KeyboardFocusTarget::X11(surface) => self.lxb.space.elements().any(|window| {
                window
                    .x11_surface()
                    .is_some_and(|candidate| x11_surface_matches(candidate, surface))
            }),
        };
        if window_is_mapped {
            return false;
        }

        let Some(focus_surface) = focus.wl_surface() else {
            return true;
        };

        if let Some(accepts) = self.layer_accepts_keyboard_focus(&focus_surface) {
            return !accepts;
        }

        true
    }

    /// Whether the layer surface `surface` belongs to will take the keyboard,
    /// or `None` when it belongs to none — an ordinary window, or a surface
    /// of nothing that is currently mapped.
    ///
    /// Answered for subsurfaces and popups as well as for the layer surface
    /// itself, since a subsurface is what a hit test can return.
    fn layer_accepts_keyboard_focus(&self, surface: &WlSurface) -> Option<bool> {
        self.lxb.space.outputs().find_map(|output| {
            let map = layer_map_for_output(output);
            let layer = map.layer_for_surface(surface, WindowSurfaceType::ALL)?;
            Some(layer.can_receive_keyboard_focus())
        })
    }

    fn focus_under_pointer(&mut self, _serial: smithay::utils::Serial) {
        let location = self.lxb.pointer_location;
        if let Some((surface, _)) = self.surface_under(location) {
            let target = self.input_target_for_surface(&surface);
            // Raise the owning window so click-to-focus also raises.
            if let Some(window) = self.window_for_input_target(&target) {
                let accepts_focus = window_accepts_keyboard_focus(&window);
                self.raise_window(&window, accepts_focus);
                if accepts_focus {
                    self.set_window_keyboard_focus(&window);
                }
            } else if click_takes_keyboard_focus(self.layer_accepts_keyboard_focus(&surface)) {
                self.set_keyboard_target(Some(target));
            }
        }
    }

    /// Re-derive keyboard focus after the window or layer stack changes.
    ///
    /// Policy, in order: an exclusive layer surface wins outright; otherwise
    /// the topmost window; otherwise an interactive layer surface. That last
    /// step is what hands focus back to the shell when the running application
    /// exits, instead of leaving the seat with nothing focused.
    pub fn focus_topmost_window(&mut self) {
        if let Some(exclusive) = self.lxb.exclusive_keyboard_focus.clone() {
            self.set_keyboard_focus(Some(exclusive));
            return;
        }

        let topmost = self
            .lxb
            .space
            .elements()
            .rev()
            .find(|window| window_accepts_keyboard_focus(window))
            .cloned();
        if let Some(window) = topmost {
            self.raise_window(&window, true);
            self.set_window_keyboard_focus(&window);
            return;
        }

        let fallback = self.interactive_layer_surface();
        self.set_keyboard_focus(fallback);
    }

    /// Apply the regular focus policy, restricted to a specific output. This
    /// is used by the multi-window X11 backend when the host focuses one of
    /// its virtual-output windows.
    fn focus_topmost_on_output(&mut self, output: &Output) {
        if let Some(exclusive) = self.lxb.exclusive_keyboard_focus.clone() {
            self.set_keyboard_focus(Some(exclusive));
            return;
        }

        let topmost = self
            .lxb
            .space
            .elements_for_output(output)
            .rev()
            .find(|window| window_accepts_keyboard_focus(window))
            .cloned();
        if let Some(window) = topmost {
            self.raise_window(&window, true);
            self.set_window_keyboard_focus(&window);
            return;
        }

        self.set_keyboard_focus(self.interactive_layer_surface_for_output(output));
    }

    /// The output whose surface currently holds keyboard focus.
    ///
    /// This is the compositor's answer to "which display is the user on". A
    /// shell drawing one layer surface per display asks for the keyboard on
    /// only the one it is driving, so this follows the user between screens
    /// without needing a pointer or a protocol of its own.
    pub fn keyboard_focus_output(&self) -> Option<Output> {
        let keyboard = self.lxb.seat.get_keyboard()?;
        let surface = keyboard.current_focus()?.wl_surface()?.into_owned();

        // Through the window's own display rather than the space's idea of
        // which outputs it overlaps, so that a window focused before it has
        // drawn anything still answers with the display it was put on. Still
        // only where that is actually known: a guess here would shut out the
        // pointer, which is a better answer than the first display.
        if let Some(window) = self.lxb.window_for_surface(&surface) {
            if let Some(output) = self.lxb.outputs.window_display(&self.lxb.space, &window) {
                return Some(output);
            }
        }

        self.lxb
            .space
            .outputs()
            .find(|output| {
                layer_map_for_output(output)
                    .layer_for_surface(&surface, WindowSurfaceType::ALL)
                    .is_some()
            })
            .cloned()
    }

    /// Topmost layer surface willing to take keyboard input, searched from the
    /// overlay layer down.
    pub fn interactive_layer_surface(&self) -> Option<WlSurface> {
        for output in self.lxb.space.outputs() {
            if let Some(surface) = self.interactive_layer_surface_for_output(output) {
                return Some(surface);
            }
        }
        None
    }

    fn interactive_layer_surface_for_output(&self, output: &Output) -> Option<WlSurface> {
        let map = layer_map_for_output(output);
        for layer in [
            WlrLayer::Overlay,
            WlrLayer::Top,
            WlrLayer::Bottom,
            WlrLayer::Background,
        ] {
            if let Some(surface) = map
                .layers_on(layer)
                .rev()
                .find(|l| l.can_receive_keyboard_focus())
            {
                return Some(surface.layer_surface().wl_surface().clone());
            }
        }
        None
    }

    fn cycle_output(&mut self, direction: i32) {
        let outputs: Vec<_> = self.lxb.space.outputs().cloned().collect();
        if outputs.len() < 2 {
            return;
        }
        let current = self
            .lxb
            .outputs
            .output_at(&self.lxb.space, self.lxb.pointer_location);
        let index = current
            .and_then(|c| outputs.iter().position(|o| *o == c))
            .unwrap_or(0) as i32;
        let next = (index + direction).rem_euclid(outputs.len() as i32) as usize;
        let target = &outputs[next];

        // Warp the pointer to the centre of the target output.
        if let Some(geo) = self.lxb.space.output_geometry(target) {
            self.lxb.pointer_location = Point::from((
                geo.loc.x as f64 + geo.size.w as f64 / 2.0,
                geo.loc.y as f64 + geo.size.h as f64 / 2.0,
            ));

            // Keep the seat's pointer focus in sync with the compositor-side
            // warp. Otherwise a button pressed before physical mouse motion
            // would still be delivered to the old output's surface.
            let location = self.lxb.pointer_location;
            let hit = self.surface_under(location);
            let under = hit
                .as_ref()
                .map(|(surface, origin)| (self.input_target_for_surface(surface), *origin));
            let time = self
                .lxb
                .start_time
                .elapsed()
                .as_millis()
                .min(u32::MAX as u128) as u32;
            if let Some(pointer) = self.lxb.seat.get_pointer() {
                pointer.motion(
                    self,
                    under,
                    &MotionEvent {
                        location,
                        serial: SERIAL_COUNTER.next_serial(),
                        time,
                    },
                );
                self.activate_constraint_at(&pointer, hit.as_ref(), location);
                pointer.frame(self);
            }
        }

        // Focus the topmost window on that output.
        let window = self
            .lxb
            .space
            .elements_for_output(target)
            .rev()
            .find(|window| window_accepts_keyboard_focus(window))
            .cloned();
        if let Some(window) = window {
            self.raise_window(&window, true);
            self.set_window_keyboard_focus(&window);
        }
    }

    fn move_window_to_next_output(&mut self) {
        let Some(window) = self.focused_window() else {
            return;
        };
        let outputs: Vec<_> = self.lxb.space.outputs().cloned().collect();
        if outputs.len() < 2 {
            return;
        }
        let current = self.lxb.space.outputs_for_element(&window).first().cloned();
        let index = current
            .and_then(|c| outputs.iter().position(|o| *o == c))
            .unwrap_or(0);
        let target = &outputs[(index + 1) % outputs.len()];

        if let Some(geo) = self.lxb.space.output_geometry(target) {
            self.lxb.space.map_element(window.clone(), geo.loc, true);
            self.sync_x11_raise(&window);
            self.lxb
                .outputs
                .tile_window_on_output(&mut self.lxb.space, &window, target);
        }
    }

    fn cycle_window(&mut self) {
        let Some(output) = self
            .lxb
            .outputs
            .output_at(&self.lxb.space, self.lxb.pointer_location)
        else {
            return;
        };
        let windows: Vec<_> = self
            .lxb
            .space
            .elements_for_output(&output)
            .filter(|window| window_accepts_keyboard_focus(window))
            .cloned()
            .collect();
        if windows.len() < 2 {
            return;
        }
        // Raising the bottom-most window rotates the stack.
        let bottom = windows.first().unwrap().clone();
        self.raise_window(&bottom, true);
        self.set_window_keyboard_focus(&bottom);
    }
}

fn active_pointer_constraint(
    pointer: &smithay::input::pointer::PointerHandle<LxbState>,
    surface: &WlSurface,
    local_location: Point<f64, Logical>,
) -> Option<ActivePointerConstraint> {
    let mut active = None;
    with_pointer_constraint(surface, pointer, |constraint| {
        let Some(constraint) = constraint.filter(|constraint| constraint.is_active()) else {
            return;
        };
        if constraint
            .region()
            .is_some_and(|region| !region.contains(local_location.to_i32_round()))
        {
            return;
        }
        active = Some(match &*constraint {
            PointerConstraint::Locked(_) => ActivePointerConstraint::Locked,
            PointerConstraint::Confined(confined) => {
                ActivePointerConstraint::Confined(confined.region().cloned())
            }
        });
    });
    active
}

fn confined_motion_is_valid(
    constraint: Option<&ActivePointerConstraint>,
    old_hit: Option<&(WlSurface, Point<f64, Logical>)>,
    new_hit: Option<&(WlSurface, Point<f64, Logical>)>,
    new_location: Point<f64, Logical>,
) -> bool {
    let Some(ActivePointerConstraint::Confined(region)) = constraint else {
        return true;
    };
    let (Some((old_surface, _)), Some((new_surface, new_origin))) = (old_hit, new_hit) else {
        return false;
    };
    old_surface == new_surface
        && region
            .as_ref()
            .map(|region| region.contains((new_location - *new_origin).to_i32_round()))
            .unwrap_or(true)
}

/// Whether a mapped desktop window is an application target rather than
/// non-interactive X11 chrome. Override-redirect games with normal window
/// types remain focusable; menus, notifications and similar transient UI do
/// not black-hole the seat.
///
/// `WM_HINTS input=false` alone is deliberately not rejected: ICCCM globally
/// active clients combine that hint with `WM_TAKE_FOCUS`, and Smithay's X11
/// keyboard target implements the required protocol handshake.
pub(crate) fn window_is_x11_chrome(window: &Window) -> bool {
    let Some(surface) = window.x11_surface() else {
        return false;
    };

    matches!(
        surface.window_type(),
        Some(
            WmWindowType::DropdownMenu
                | WmWindowType::Menu
                | WmWindowType::Notification
                | WmWindowType::PopupMenu
                | WmWindowType::Splash
                | WmWindowType::Toolbar
                | WmWindowType::Tooltip
        )
    )
}

pub(crate) fn window_accepts_keyboard_focus(window: &Window) -> bool {
    !window_is_x11_chrome(window) && x11_window_accepts_input(window)
}

/// Whether a click or a touch on a surface may hand it the keyboard.
///
/// `layer` is what the layer surface it belongs to asked for — `Some(false)`
/// where that is `keyboard_interactivity: none` — and `None` where it belongs
/// to no layer surface at all, which is every ordinary window and is focused
/// exactly as it was before.
///
/// The case that matters is a layer surface that asked for none of the
/// keyboard: it means it, and being clicked is not a change of mind. That is
/// the shell's on-screen keyboard. The board is a picture of a keyboard drawn
/// over the application it types into, and keyboard focus is what carries
/// text-input focus with it — so a key that took focus on the way down would
/// deactivate the very field it was about to type into. The board would put
/// itself away at the first letter, and the letter would arrive at the shell
/// instead of at the application.
fn click_takes_keyboard_focus(layer: Option<bool>) -> bool {
    layer.unwrap_or(true)
}

fn absolute_position<B, E>(
    event: &E,
    target_size: Size<i32, Logical>,
    source_size: Option<Size<i32, Physical>>,
) -> Point<f64, Logical>
where
    B: InputBackend,
    E: AbsolutePositionEvent<B>,
{
    let Some(source_size) = source_size.filter(|size| size.w > 0 && size.h > 0) else {
        return event.position_transformed(target_size);
    };

    Point::from((
        map_window_coordinate(event.x(), source_size.w, target_size.w),
        map_window_coordinate(event.y(), source_size.h, target_size.h),
    ))
}

fn map_window_coordinate(value: f64, source_extent: i32, target_extent: i32) -> f64 {
    if source_extent <= 0 || target_extent <= 0 {
        return 0.0;
    }
    let upper = (target_extent as f64 - f64::EPSILON).max(0.0);
    (value / source_extent as f64 * target_extent as f64).clamp(0.0, upper)
}

/// Whether a difference between the two pointers is XWayland's doing alone.
///
/// Three things have to hold, and each rules out a way of being wrong:
///
/// * **We did not move.** XWayland learns of our motion a moment after it
///   happens, so during a real drag its pointer is always a little behind
///   ours. Following it then would haul the cursor backwards against the hand
///   moving the mouse.
/// * **They did move.** With no previous reading there is nothing to compare
///   against, and an unchanged one is agreement rather than divergence.
/// * **By at least a whole pixel.** XWayland keeps its pointer in integers
///   while ours is fractional, so the two are almost never exactly equal and a
///   sub-pixel difference would be chased for ever without arriving.
fn xwayland_moved_alone(
    ours_moved: bool,
    theirs_before: Option<Point<f64, Logical>>,
    theirs_now: Point<f64, Logical>,
    delta: Point<f64, Logical>,
) -> bool {
    !ours_moved
        && theirs_before.is_some_and(|before| before != theirs_now)
        && (delta.x.abs() >= 1.0 || delta.y.abs() >= 1.0)
}

#[cfg(test)]
mod tests {

    /// The cursor follows XWayland only when XWayland moved on its own.
    #[test]
    fn the_cursor_follows_xwayland_only_when_it_moved_alone() {
        let at = |x: f64, y: f64| Point::<f64, Logical>::from((x, y));
        let far = at(50.0, 0.0);

        // The case this exists for: an X client synthesised motion, ours sat
        // still, and the two are now a long way apart.
        assert!(xwayland_moved_alone(
            false,
            Some(at(10.0, 0.0)),
            at(60.0, 0.0),
            far
        ));

        // Our own drag. Theirs is stale behind us and must not be copied back,
        // or the cursor fights the hand.
        assert!(!xwayland_moved_alone(
            true,
            Some(at(10.0, 0.0)),
            at(60.0, 0.0),
            far
        ));

        // Nothing to compare against yet.
        assert!(!xwayland_moved_alone(false, None, at(60.0, 0.0), far));

        // Agreement is not divergence.
        assert!(!xwayland_moved_alone(
            false,
            Some(at(60.0, 0.0)),
            at(60.0, 0.0),
            at(0.0, 0.0)
        ));

        // A sub-pixel difference is rounding, not movement, and chasing it
        // would never arrive.
        assert!(!xwayland_moved_alone(
            false,
            Some(at(10.0, 0.0)),
            at(10.5, 0.0),
            at(0.5, 0.25)
        ));
        // But a whole pixel on either axis alone is enough.
        assert!(xwayland_moved_alone(
            false,
            Some(at(10.0, 0.0)),
            at(10.0, 12.0),
            at(0.0, 1.0)
        ));
    }
    use super::*;

    #[test]
    fn parses_key_patterns() {
        let p = KeyPattern::parse("Super+Q").unwrap();
        assert!(p.logo && !p.ctrl && !p.alt && !p.shift);

        let p = KeyPattern::parse("Ctrl+Alt+BackSpace").unwrap();
        assert!(p.ctrl && p.alt);
        assert_eq!(p.keysym.raw(), keysyms::KEY_BackSpace);

        assert!(KeyPattern::parse("Hyper+Q").is_none());
        assert!(KeyPattern::parse("Super+NotAKey").is_none());
    }

    #[test]
    fn letter_bindings_match_the_key_that_was_pressed() {
        // Pressing Q without shift produces the keysym `q`, so a binding
        // written `Super+Q` has to match that or it never fires at all.
        let pattern = KeyPattern::parse("Super+Q").unwrap();
        let mods = ModifiersState {
            logo: true,
            ..Default::default()
        };
        assert!(pattern.matches(&mods, Keysym::from(keysyms::KEY_q)));
        assert!(pattern.matches(&mods, Keysym::from(keysyms::KEY_Q)));

        // Spelling it in lowercase means the same key.
        assert_eq!(KeyPattern::parse("Super+q"), Some(pattern));

        // Shift is still a modifier in its own right, so the two bindings stay
        // distinct and neither answers for the other.
        let shifted = KeyPattern::parse("Super+Shift+Q").unwrap();
        assert_ne!(shifted, pattern);
        assert!(!shifted.matches(&mods, Keysym::from(keysyms::KEY_q)));
        let shifted_mods = ModifiersState {
            logo: true,
            shift: true,
            ..Default::default()
        };
        assert!(!pattern.matches(&shifted_mods, Keysym::from(keysyms::KEY_Q)));
        assert!(shifted.matches(&shifted_mods, Keysym::from(keysyms::KEY_Q)));
    }

    /// The screenshot chord is written with the digit on it, and the digit is
    /// not what the keyboard produces while shift is held: on a US layout
    /// `Shift+3` is `numbersign`, on a UK one it is `sterling`, and on neither
    /// of them is it `3`. The binding matches because every spelling of the
    /// key is tried, the unshifted one included — which is the whole reason
    /// [`KeyBindings::lookup`] takes a list rather than one symbol.
    #[test]
    fn the_screenshot_chord_matches_the_key_under_the_shift() {
        let bindings = KeyBindings::from_config(&Config::default());
        let mods = ModifiersState {
            ctrl: true,
            shift: true,
            ..Default::default()
        };
        let pressed = [
            Keysym::from(keysyms::KEY_numbersign),
            Keysym::from(keysyms::KEY_3),
        ];
        assert_eq!(bindings.lookup(&mods, pressed), Some(Action::Screenshot));

        // The digit on its own is a digit. Nothing about a screenshot may
        // happen while somebody is typing a number.
        assert_eq!(
            bindings.lookup(&ModifiersState::default(), [Keysym::from(keysyms::KEY_3)]),
            None
        );
        // And the key with a picture of it printed on the keyboard, which is
        // what a hand reaches for without being told the chord.
        assert_eq!(
            bindings.lookup(
                &ModifiersState::default(),
                [Keysym::from(keysyms::KEY_Print)]
            ),
            Some(Action::Screenshot)
        );

        // The Mac chord under the other reading of the Command key: the one
        // that sits where Command sits, rather than the one that does what it
        // does. Both hands are right.
        let held = ModifiersState {
            alt: true,
            shift: true,
            ..Default::default()
        };
        assert_eq!(bindings.lookup(&held, pressed), Some(Action::Screenshot));
    }

    /// The screenshot key answers whatever is held down with it.
    ///
    /// Every desktop spells a *variant* of the picture with a modifier — the
    /// whole screen on `Shift+Print`, the clipboard on `Ctrl+Print`, a region
    /// on `Meta+Shift+Print` — and this shell takes one kind of picture. A
    /// user whose hand knows one of those spellings must not press the key and
    /// have nothing happen.
    #[test]
    fn the_screenshot_key_does_not_care_what_is_held_with_it() {
        let bindings = KeyBindings::from_config(&Config::default());
        let print = [Keysym::from(keysyms::KEY_Print)];
        for held in [
            ModifiersState {
                shift: true,
                ..Default::default()
            },
            ModifiersState {
                ctrl: true,
                ..Default::default()
            },
            ModifiersState {
                logo: true,
                shift: true,
                ..Default::default()
            },
        ] {
            assert_eq!(bindings.lookup(&held, print), Some(Action::Screenshot));
        }

        // Loose is not contagious: the chord written with a digit still needs
        // its modifiers, because the digit belongs to whoever is typing.
        assert_eq!(
            bindings.lookup(
                &ModifiersState {
                    logo: true,
                    ..Default::default()
                },
                [Keysym::from(keysyms::KEY_3)]
            ),
            None
        );
    }

    /// And a user who writes down what the key does has said it about the key,
    /// not about one decoration of it — so the built-in stops answering for the
    /// rest.
    #[test]
    fn a_screenshot_key_given_away_in_the_config_is_given_away_entirely() {
        let mut config = Config::default();
        config
            .keybindings
            .insert("Print".into(), "spawn:grim".into());
        let bindings = KeyBindings::from_config(&config);

        let print = [Keysym::from(keysyms::KEY_Print)];
        assert_eq!(
            bindings.lookup(&ModifiersState::default(), print),
            Some(Action::Spawn("grim".into()))
        );
        assert_eq!(
            bindings.lookup(
                &ModifiersState {
                    shift: true,
                    ..Default::default()
                },
                print
            ),
            None,
            "the key is theirs now, decorated or not"
        );
    }

    /// Every binding in the table is one the config file can move elsewhere —
    /// the guide's chords excepted, which have a test of their own above.
    #[test]
    fn the_screenshot_chord_can_be_given_to_something_else() {
        let mut config = Config::default();
        config
            .keybindings
            .insert("Ctrl+Shift+3".into(), "spawn:grim".into());
        let bindings = KeyBindings::from_config(&config);

        let mods = ModifiersState {
            ctrl: true,
            shift: true,
            ..Default::default()
        };
        assert_eq!(
            bindings.lookup(&mods, [Keysym::from(keysyms::KEY_3)]),
            Some(Action::Spawn("grim".into())),
            "a chord the user has claimed is theirs"
        );
    }

    #[test]
    fn case_folding_leaves_non_letters_alone() {
        for raw in [keysyms::KEY_Home, keysyms::KEY_F1, keysyms::KEY_bracketleft] {
            assert_eq!(fold_case(Keysym::from(raw)).raw(), raw);
        }
    }

    #[test]
    fn parses_actions() {
        assert_eq!(Action::parse("quit"), Some(Action::Quit));
        assert_eq!(
            Action::parse("spawn:foot -e htop"),
            Some(Action::Spawn("foot -e htop".into()))
        );
        assert_eq!(Action::parse("vt:3"), Some(Action::SwitchVt(3)));
        assert_eq!(Action::parse("nonsense"), None);
    }

    /// The guide is how a user gets back out of a fullscreen application. A
    /// configuration file that quietly took its chord for something else would
    /// leave a session with no way home, so the chord is refused instead.
    #[test]
    fn nothing_can_take_the_guide_chord_away() {
        let mut config = Config::default();
        config
            .keybindings
            .insert("Super+Home".into(), "spawn:foot".into());
        let bindings = KeyBindings::from_config(&config);

        let mods = ModifiersState {
            logo: true,
            ..Default::default()
        };
        assert_eq!(
            bindings.lookup(&mods, [Keysym::from(keysyms::KEY_Home)]),
            Some(Action::Guide)
        );
    }

    /// The Windows key on its own is the home button: pressed, let go of, and
    /// nothing in between.
    #[test]
    fn the_windows_key_summons_the_guide_when_it_is_let_go_of_alone() {
        let mut tap = HomeTap::default();

        // The press itself does nothing. It is also the start of every chord
        // in the table, and a guide that opened here would shadow all of them.
        tap.key(true, true);
        assert!(!tap.take());
        tap.key(true, false);
        assert!(tap.take(), "letting it go alone is the home button");

        // Taken once, not once per frame afterwards.
        assert!(!tap.take());
    }

    /// `Super+Q` is `Super+Q`, not the guide followed by a closed window.
    #[test]
    fn a_chord_is_not_a_tap_however_it_ends() {
        let mut tap = HomeTap::default();
        tap.key(true, true);
        tap.key(false, true);
        assert!(!tap.take());
        // Neither edge of the other key brings the tap back.
        tap.key(false, false);
        assert!(!tap.take());
        tap.key(true, false);
        assert!(!tap.take());

        // And a Windows key pressed after the chord has been let go of is a
        // fresh tap: the hand went back to it deliberately.
        tap.key(true, true);
        tap.key(true, false);
        assert!(tap.take());
    }

    /// A key of some other name, pressed and released on its own, is not a tap
    /// of a key that was never touched.
    #[test]
    fn an_ordinary_key_on_its_own_summons_nothing() {
        let mut tap = HomeTap::default();
        tap.key(false, true);
        assert!(!tap.take());
        tap.key(false, false);
        assert!(!tap.take());
    }

    /// A hand on the mouse, or the keys being taken off the session, ends the
    /// tap that was waiting to complete — a release arriving after either of
    /// those belongs to something else.
    #[test]
    fn an_interruption_ends_the_tap() {
        let mut tap = HomeTap::default();
        tap.key(true, true);
        tap.interrupt();
        tap.key(true, false);
        assert!(!tap.take());
    }

    /// Both names of the key are the key.
    #[test]
    fn either_windows_key_is_the_home_button() {
        assert!(is_logo_key(&[Keysym::from(keysyms::KEY_Super_L)]));
        assert!(is_logo_key(&[Keysym::from(keysyms::KEY_Super_R)]));
        assert!(!is_logo_key(&[Keysym::from(keysyms::KEY_q)]));
        assert!(!is_logo_key(&[]));
    }

    /// A chord the user adds for the guide gets the same protection, and stops
    /// being whatever it was bound to before.
    #[test]
    fn a_configured_guide_chord_joins_the_protected_ones() {
        let mut config = Config::default();
        // Super+K is the on-screen keyboard by default.
        config.keybindings.insert("Super+K".into(), "guide".into());
        let bindings = KeyBindings::from_config(&config);

        let mods = ModifiersState {
            logo: true,
            ..Default::default()
        };
        assert_eq!(
            bindings.lookup(&mods, [Keysym::from(keysyms::KEY_k)]),
            Some(Action::Guide)
        );
        // And the built-in chords are still there beside it.
        assert_eq!(
            bindings.lookup(&mods, [Keysym::from(keysyms::KEY_Home)]),
            Some(Action::Guide)
        );
    }

    /// Every spelling of the key is tried against the guide before any of them
    /// is tried against anything else, so a layout that renders the pressed key
    /// as some other binding's symbol cannot get in ahead of it.
    #[test]
    fn the_guide_outranks_a_binding_on_another_spelling_of_the_same_key() {
        let mut config = Config::default();
        config.keybindings.insert("Super+X".into(), "quit".into());
        let bindings = KeyBindings::from_config(&config);

        let mods = ModifiersState {
            logo: true,
            ..Default::default()
        };
        // Modified symbol first, then the raw one: the guide still wins.
        assert_eq!(
            bindings.lookup(
                &mods,
                [
                    Keysym::from(keysyms::KEY_x),
                    Keysym::from(keysyms::KEY_Home)
                ]
            ),
            Some(Action::Guide)
        );
    }

    #[test]
    fn config_overrides_default_binding() {
        let mut config = Config::default();
        config
            .keybindings
            .insert("Super+Q".into(), "spawn:foot".into());
        let bindings = KeyBindings::from_config(&config);

        let pattern = KeyPattern::parse("Super+Q").unwrap();
        let matched: Vec<_> = bindings
            .bindings
            .iter()
            .filter(|(p, _)| *p == pattern)
            .collect();
        assert_eq!(matched.len(), 1, "binding should not be duplicated");
        assert_eq!(matched[0].1, Action::Spawn("foot".into()));
    }

    /// Clicking a key on the on-screen keyboard must leave the keys where they
    /// are. The board is drawn over the application it types into and asks for
    /// no keyboard of its own; taking focus from the click would deactivate the
    /// text field that summoned it, and the board would close on the first
    /// letter instead of typing it.
    #[test]
    fn a_layer_surface_that_declined_the_keyboard_is_not_given_it_by_a_click() {
        assert!(!click_takes_keyboard_focus(Some(false)));
        // An exclusive or on-demand layer surface is still click-to-focus,
        // which is how the launcher's own bar is reached.
        assert!(click_takes_keyboard_focus(Some(true)));
        // And a surface belonging to no layer surface — every window — is
        // focused exactly as it was.
        assert!(click_takes_keyboard_focus(None));
    }

    #[test]
    fn maps_rectangular_nested_touch_coordinates_per_axis() {
        // Regresses Smithay 0.7's winit touch normalisation bug, where Y is
        // divided by the host window width. Raw window coordinates must use
        // the matching source extent for each axis.
        assert_eq!(map_window_coordinate(480.0, 960, 1920), 960.0);
        assert_eq!(map_window_coordinate(300.0, 600, 1200), 600.0);
    }
}
