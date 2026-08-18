//! Input routing and compositor keybindings.

use std::time::Duration;

use lxb_protocol::server::lxb_shell_v1::VolumeChange;
use smithay::backend::input::{
    AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event, InputBackend, InputEvent,
    KeyState, KeyboardKeyEvent, Keycode, PointerAxisEvent, PointerButtonEvent, PointerMotionEvent,
    TouchEvent,
};
use smithay::desktop::{layer_map_for_output, Window, WindowSurfaceType};
use smithay::input::keyboard::{keysyms, xkb, FilterResult, Keysym, ModifiersState};
use smithay::input::pointer::{AxisFrame, ButtonEvent, MotionEvent, RelativeMotionEvent};
use smithay::input::touch::{DownEvent, MotionEvent as TouchMotionEvent, UpEvent};
use smithay::output::Output;
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{IsAlive, Logical, Physical, Point, Size, SERIAL_COUNTER};
use smithay::wayland::compositor::RegionAttributes;
use smithay::wayland::pointer_constraints::{with_pointer_constraint, PointerConstraint};
use smithay::wayland::seat::WaylandFocus;
use smithay::wayland::shell::wlr_layer::Layer as WlrLayer;
use smithay::xwayland::xwm::WmWindowType;
use smithay::xwayland::X11Surface;

use crate::config::Config;
use crate::focus::{x11_surface_matches, KeyboardFocusTarget};
use crate::render::same_application;
use crate::state::LxbState;
use crate::xwayland::x11_window_accepts_input;

#[derive(Clone)]
enum ActivePointerConstraint {
    Locked,
    Confined(Option<RegionAttributes>),
}

/// What the pointer or a finger is over, and in whose coordinates.
///
/// Three things rather than the surface and its origin, because the two are not
/// enough on their own: an application drawing larger than life works in a
/// smaller coordinate space than the screen does — see [`crate::scale`] — and a
/// press has to be delivered in that space. So the point is carried here
/// already converted, alongside the step it was converted by, and every caller
/// hands the seat [`Hit::point`] rather than the location it started with.
///
/// The origin is the surface's own place in whichever space the point is in, so
/// `point - origin` is what the client is told about and needs no further
/// arithmetic. That is also what lets a scaled window be delivered through
/// smithay's seat unchanged: what it wants is a location and an origin, and
/// these are that pair — measured in the client's space instead of the screen's.
#[derive(Clone)]
pub struct Hit {
    /// The surface the point landed on: a window's, one of its subsurfaces or
    /// popups, or one of the shell's own layer surfaces.
    pub surface: WlSurface,
    /// Where that surface's origin is, in the same coordinates as [`Self::point`].
    pub origin: Point<f64, Logical>,
    /// Where the press landed, in the coordinates the surface's client works
    /// in. The screen's own for everything but a scaled application's windows.
    pub point: Point<f64, Logical>,
    /// The step between the two spaces, for going back the other way.
    pub mapping: crate::scale::Mapping,
}

impl Hit {
    /// Where in the surface the press landed, which is what a client is told.
    fn local(&self) -> Point<f64, Logical> {
        self.point - self.origin
    }
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
    /// Ask the session shell to set the volume, or to silence it.
    ///
    /// A compositor binding for the fourth time, and here the application in
    /// front is not merely holding the key — it is the thing being turned
    /// down. A volume key that stopped working the moment a game took the
    /// keyboard would be a volume key for the launcher only.
    ///
    /// Forwarded rather than acted on, like the screenshot: the compositor has
    /// the key, and the shell has the mixer and somewhere to draw it.
    Volume(VolumeChange),
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
            "volume-up" => Action::Volume(VolumeChange::Up),
            "volume-down" => Action::Volume(VolumeChange::Down),
            "volume-mute" | "mute" => Action::Volume(VolumeChange::Mute),
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
/// Neither edge reaches the application, and that is the point rather than a
/// side effect. The home button belongs to the shell and to the compositor and
/// to nothing else: a console's home button is the one control that means the
/// same thing whatever is on screen, and an application that can see it is an
/// application that can act on it — Valve's client takes it for an overlay of
/// its own, and a game takes it for whatever the Windows key does in that game.
///
/// It used to forward both edges, on the reasoning that swallowing only the
/// release would leave the application holding a Super it never sees let go of.
/// That reasoning is sound, and is exactly why *both* go now: a key the client
/// never learns about cannot be stuck down. Chords are untouched — the modifier
/// state belongs to the seat and is updated before any of this, so `Super+Q`
/// still resolves; what an application loses is the Windows key by itself.
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

/// The volume key the user is holding down.
///
/// A binding acts once and is done with — a guide button held down is a guide
/// button pressed once. A volume key is the exception, and not by choice:
/// the control moves a twentieth of its range per press, so crossing it means
/// twenty presses or one key held, and every machine anybody has ever used
/// does the second. Nothing else can supply it either. A client repeats keys
/// itself, from the rate the seat hands it — and this key never reaches a
/// client, because the compositor swallowed it on the way to the application
/// underneath. So the repeat is the compositor's, at the same rate.
///
/// Which press a step belongs to is a number rather than a comparison of what
/// is held: the timer for a key that has been let go of cannot be taken out of
/// the loop from outside its own callback, so it is left to expire and asks on
/// the way past whether anybody still wants it. A press taken *since* answers
/// no, which is what keeps a key pressed twice quickly from ending up with two
/// timers stepping the volume together.
#[derive(Debug, Default)]
pub struct VolumeKey {
    /// The key that is down, and the number its steps are booked under.
    held: Option<(Keycode, u64)>,
    /// The last number handed out, so no two presses share one.
    generation: u64,
}

impl VolumeKey {
    /// Note a key going down, and take the number to book its steps under.
    fn pressed(&mut self, keycode: Keycode) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.held = Some((keycode, self.generation));
        self.generation
    }

    /// Note a key coming up.
    ///
    /// Only the key that owns the repeat can end it. Turning the volume up and
    /// then down without letting go of the first key hands the repeat to the
    /// second, and the release of the first must not stop the one now running.
    fn released(&mut self, keycode: Keycode) {
        if self.held.is_some_and(|(held, _)| held == keycode) {
            self.held = None;
        }
    }

    /// Whether the step booked under `generation` is still wanted.
    fn wants(&self, generation: u64) -> bool {
        self.held.is_some_and(|(_, booked)| booked == generation)
    }

    /// Whatever was being held, it is not being held now.
    ///
    /// The session losing the keys, on the same terms as [`HomeTap::interrupt`]
    /// and for a louder reason: a repeat that survived a VT switch would go on
    /// turning the volume down on a session the user has left.
    pub fn interrupt(&mut self) {
        self.held = None;
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
            // The three keys with a speaker printed on them, and they are
            // loose for the same reason `Print` is: each is a key with one job
            // rather than a letter with something held down. They are reached
            // through Fn on most laptops and through the media row on most
            // keyboards, neither of which agrees with the other about what
            // else is being held at the time.
            ("Any+XF86AudioRaiseVolume", Action::Volume(VolumeChange::Up)),
            (
                "Any+XF86AudioLowerVolume",
                Action::Volume(VolumeChange::Down),
            ),
            ("Any+XF86AudioMute", Action::Volume(VolumeChange::Mute)),
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
            let logo = is_logo_key(&handle.raw_syms());
            state.lxb.home_tap.key(logo, pressed);

            // The modified symbol as well as the raw ones, so that both
            // `Super+Q` and shift-rewritten combos like `Super+Shift+Right`
            // resolve to the same binding.
            let candidates = std::iter::once(handle.modified_sym()).chain(handle.raw_syms());
            match state.lxb.keybindings.lookup(mods, candidates) {
                Some(action) => FilterResult::Intercept(Some(action)),
                // The home button is the shell's and the compositor's, and no
                // application is told it was pressed — see [`HomeTap`]. Held,
                // it is still the modifier half of every chord in the table;
                // that is the seat's own state and is not affected by this.
                None if logo => FilterResult::Intercept(None),
                None => FilterResult::Forward,
            }
        });
        let action = action.flatten();

        // A key was pressed on a keyboard the compositor can see, which is the
        // user telling it they are not using the mouse. Done for every key,
        // including the ones that turn out to be bindings: the guide button is
        // as much a hand off the mouse as a letter is.
        //
        // And the shell is told the same thing about the control it holds
        // instead: a hand on the keyboard is a hand off the controller, and the
        // shell has a corner of the screen offering that controller a keyboard.
        // It cannot see this for itself while an application owns the keys,
        // which is the case the offer is made in. Sent once until the shell says
        // the pad is back — see `ShellControlState::typing_is_news`.
        if pressed {
            self.pointer_put_down();
            self.lxb.shell_control.send_typed();
        }

        // Actions fire on press only; the matching release is swallowed too,
        // which is what clients expect from a grabbed binding.
        //
        // Except the volume keys, which are the one binding that goes on
        // meaning something while it is held: they act on the press like
        // everything else and then keep stepping until the key comes back up,
        // so the release is the only thing that can stop them. See
        // [`VolumeKey`].
        match action {
            Some(Action::Volume(change)) => self.volume_key(change, keycode, pressed),
            Some(action) if pressed => self.run_action(action),
            _ => {}
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
            Action::Volume(change) => self.change_volume(change),
        }
    }

    /// One volume key going down or coming up.
    ///
    /// The press is forwarded at once, and the key then holds the repeat until
    /// it is let go of — which is what makes crossing the whole range one held
    /// key rather than twenty presses.
    ///
    /// Mute is not stepped and takes no repeat: it is a switch, and a switch
    /// held down is a switch thrown once. Holding it would flip the session
    /// between silent and loud twenty-five times a second.
    fn volume_key(&mut self, change: VolumeChange, keycode: Keycode, pressed: bool) {
        if !pressed {
            self.lxb.volume_key.released(keycode);
            return;
        }
        self.run_action(Action::Volume(change));
        if change != VolumeChange::Mute {
            self.repeat_volume_key(change, keycode);
        }
    }

    /// Book the steps a held volume key owes, at the seat's own repeat rate.
    ///
    /// The user's rate, from the same two settings every key in the session
    /// repeats by: this is a key on their keyboard, and one that walked a bar
    /// at a pace of the compositor's own choosing would be the one key on the
    /// machine that ignores what they asked for. A rate of nothing turns the
    /// repeat off, exactly as it does for every other key.
    fn repeat_volume_key(&mut self, change: VolumeChange, keycode: Keycode) {
        let input = &self.lxb.config.input;
        if input.repeat_rate <= 0 {
            return;
        }
        let delay = Duration::from_millis(input.repeat_delay.max(0) as u64);
        let interval = Duration::from_secs_f64(1.0 / f64::from(input.repeat_rate));
        let booked = self.lxb.volume_key.pressed(keycode);

        let timer = Timer::from_duration(delay);
        let insert =
            self.lxb
                .loop_handle
                .insert_source(timer, move |_, _, state: &mut LxbState| {
                    // Asked every time rather than once: the key may have come up,
                    // or another may have taken the repeat over, between one step
                    // and the next.
                    if !state.lxb.volume_key.wants(booked) {
                        return TimeoutAction::Drop;
                    }
                    state.run_action(Action::Volume(change));
                    TimeoutAction::ToDuration(interval)
                });
        if let Err(err) = insert {
            // The press itself has already been forwarded; what is lost is the
            // holding, so the key is a key that steps once.
            tracing::warn!(?err, "no timer for a held volume key");
        }
    }

    /// Ask a window to close itself, whichever shell it speaks.
    ///
    /// This is a request, not a kill: the client may prompt about unsaved work
    /// or ignore it entirely.
    pub fn request_window_close(&mut self, window: &Window) {
        // Nothing can act on a request while it is stopped, and an application
        // behind the start screen is exactly the one Close is pressed on. See
        // [`LxbState::wake_this_application`].
        self.wake_this_application(window);
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

    /// The arrow has moved, so the picture has changed.
    ///
    /// Nothing else is going to say so. This compositor draws on demand — a
    /// display whose render state is idle draws nothing until something asks —
    /// and the cursor is the one thing on screen that moves without any client
    /// committing anything. On the shell that never showed, because the start
    /// screen animates continuously and the cursor rode along with it. Over an
    /// application it is the whole bug: LineXinBar gives a game the entire
    /// display, the shell behind it stops drawing altogether, and a game with
    /// nothing to redraw — a point-and-click waiting for the very motion being
    /// delivered — commits nothing either. So the arrow stayed where it was
    /// last painted while the pointer went on moving underneath it: clicks
    /// landed where the user was really pointing, and the thing they were
    /// aiming with sat still. On a session that had just handed a game the
    /// screen, where it was last painted is where the pointer starts, which is
    /// the corner.
    fn cursor_moved(&mut self) {
        self.queue_redraw();
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
        self.pointer_moved_by(delta, delta_unaccel, utime, time_msec, RelativeStream::Send);
    }

    /// The same movement, saying whether the client hears it on the relative
    /// stream as well. See [`RelativeStream`].
    fn pointer_moved_by(
        &mut self,
        delta: Point<f64, Logical>,
        delta_unaccel: Point<f64, Logical>,
        utime: u64,
        time_msec: u32,
        relative: RelativeStream,
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
            .map(|hit| (self.input_target_for_surface(&hit.surface), hit.origin));
        let constraint = old_hit
            .as_ref()
            .and_then(|hit| active_pointer_constraint(&pointer, &hit.surface, hit.local()));

        // Relative motion is delivered even while the logical pointer is
        // locked. Games use this stream for camera movement while wl_pointer
        // remains stationary.
        //
        // In the same space as the motion beside it, which on a scaled
        // application is that window's rather than the screen's: a game whose
        // camera came off this stream at the screen's scale would turn further
        // per inch of mouse than its own cursor moved.
        let travel = old_hit
            .as_ref()
            .map(|hit| hit.mapping)
            .unwrap_or_else(crate::scale::Mapping::none);
        if relative == RelativeStream::Send {
            pointer.relative_motion(
                self,
                old_under.clone(),
                &RelativeMotionEvent {
                    delta: travel.delta_into_window(delta),
                    delta_unaccel: travel.delta_into_window(delta_unaccel),
                    utime,
                },
            );
        }

        if matches!(constraint, Some(ActivePointerConstraint::Locked)) {
            pointer.frame(self);
            return;
        }

        self.lxb.pointer_location = old_location + delta;
        self.clamp_pointer();
        let location = self.lxb.pointer_location;
        let hit = self.surface_under(location);

        let valid = confined_motion_is_valid(constraint.as_ref(), old_hit.as_ref(), hit.as_ref());
        watch_a_refused_pointer(&mut self.lxb, !valid, old_location);
        if !valid {
            self.lxb.pointer_location = old_location;
            pointer.frame(self);
            return;
        }

        // The location handed over is the one the surface's own client works
        // in — see [`Hit`] — and it is paired with the origin from the same
        // hit, so what smithay computes from the two is the point inside the
        // surface. Nothing is delivered where nothing was hit, and there the
        // screen's own coordinate is as good as any.
        let (under, delivered) = match hit.as_ref() {
            Some(hit) => (
                Some((self.input_target_for_surface(&hit.surface), hit.origin)),
                hit.point,
            ),
            None => (None, location),
        };

        pointer.motion(
            self,
            under.clone(),
            &MotionEvent {
                location: delivered,
                serial,
                time: time_msec,
            },
        );
        self.activate_constraint_at(&pointer, hit.as_ref());
        pointer.frame(self);
        self.cursor_moved();
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
            .map(|hit| (self.input_target_for_surface(&hit.surface), hit.origin));
        let constraint = old_hit
            .as_ref()
            .and_then(|hit| active_pointer_constraint(&pointer, &hit.surface, hit.local()));

        // Nested backends only expose host cursor coordinates. Deriving a
        // delta here keeps wp_relative_pointer useful for nested testing; the
        // native libinput backend supplies true unaccelerated relative events.
        if let Some(delta) = nested_delta.filter(|delta| delta.x != 0.0 || delta.y != 0.0) {
            // In the surface's own space, as in [`Self::pointer_motion_by`].
            let travel = old_hit
                .as_ref()
                .map(|hit| hit.mapping)
                .unwrap_or_else(crate::scale::Mapping::none);
            let delta = travel.delta_into_window(delta);
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
        let valid = confined_motion_is_valid(constraint.as_ref(), old_hit.as_ref(), hit.as_ref());
        watch_a_refused_pointer(&mut self.lxb, !valid, old_location);
        if !valid {
            self.lxb.pointer_location = old_location;
            pointer.frame(self);
            return;
        }

        // Delivered in the surface's own coordinates; see the same pair in
        // [`Self::pointer_motion_by`].
        let (under, delivered) = match hit.as_ref() {
            Some(hit) => (
                Some((self.input_target_for_surface(&hit.surface), hit.origin)),
                hit.point,
            ),
            None => (None, location),
        };
        pointer.motion(
            self,
            under,
            &MotionEvent {
                location: delivered,
                serial,
                time: event.time_msec(),
            },
        );
        self.activate_constraint_at(&pointer, hit.as_ref());
        pointer.frame(self);
        self.cursor_moved();
    }

    fn activate_constraint_at(
        &mut self,
        pointer: &smithay::input::pointer::PointerHandle<Self>,
        hit: Option<&Hit>,
    ) {
        let Some(hit) = hit else {
            return;
        };
        // A constraint's region is the client's own, so the point compared
        // against it has to be the client's own too — which is what a [`Hit`]
        // already carries.
        let local = hit.local();
        with_pointer_constraint(&hit.surface, pointer, |constraint| {
            if let Some(constraint) = constraint {
                let inside = constraint
                    .region()
                    .map(|region| region.contains(local.to_i32_round()))
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

        let Some(hit) = self.surface_under(self.lxb.pointer_location) else {
            self.lxb.pointer_position_hint = None;
            return;
        };
        if hit.surface != surface {
            self.lxb.pointer_position_hint = None;
            return;
        }

        // The hint is a place in the client's own surface, and where the
        // pointer is kept is a place on the screen. On a scaled application
        // those are two different spaces, so the answer has to come back out of
        // the window's before it is believed.
        let from = self.lxb.pointer_location;
        self.lxb.pointer_location = hit.mapping.onto_screen(hit.origin + hint);
        self.clamp_pointer();
        let to = self.lxb.pointer_location;
        watch_a_client_placing_the_pointer(&mut self.lxb, from, to);
        self.lxb.pointer_position_hint = None;
        self.cursor_moved();
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
        // Withheld from the relative stream: this is us catching up with a
        // pointer XWayland has already moved, not a hand moving a mouse. See
        // [`RelativeStream::Withhold`] for what sending it here did.
        self.pointer_moved_by(
            delta,
            delta,
            u64::from(time) * 1000,
            time,
            RelativeStream::Withhold,
        );
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

    /// Re-derive what the pointer is sitting on, without moving it.
    ///
    /// Pointer focus normally follows the pointer, and a pointer that has not
    /// moved is one nothing needs to be said about. That is only true while
    /// what is *under* it holds still. When the shell hands a display back to
    /// an application — its start screen leaves the overlay layer, or a window
    /// is asked back to the front — the surface under an untouched pointer
    /// changes without a single pointer event to notice it by, and the
    /// application it changed to hears nothing: no enter, no cursor, and no
    /// chance to have the pointer lock it asks for on the way back granted,
    /// since a lock is only activated for the surface the pointer is on.
    ///
    /// What that looked like: a game brought back from its cover appeared,
    /// filled the screen, and then sat there ignoring the pad. It came alive on
    /// the first click of a mouse — which is not a fix but the diagnosis, since
    /// a click is the one thing that re-derives this, and it is also the one
    /// thing a controller cannot do.
    pub fn refresh_pointer_focus(&mut self) {
        let Some(pointer) = self.lxb.seat.get_pointer() else {
            return;
        };
        self.ensure_pointer_focus(&pointer);
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
        let wanted = hit.as_ref().map(|hit| hit.surface.clone());
        let current = pointer
            .current_focus()
            .and_then(|focus| focus.wl_surface().map(|surface| surface.into_owned()));
        if current == wanted {
            return;
        }

        let (under, delivered) = match hit.as_ref() {
            Some(hit) => (
                Some((self.input_target_for_surface(&hit.surface), hit.origin)),
                hit.point,
            ),
            None => (None, location),
        };
        let time = self.monotonic_msec();
        pointer.motion(
            self,
            under,
            &MotionEvent {
                location: delivered,
                serial: SERIAL_COUNTER.next_serial(),
                time,
            },
        );
        self.activate_constraint_at(pointer, hit.as_ref());
        pointer.frame(self);
    }

    /// Whether any application window is mapped at all.
    ///
    /// One nobody can see does not count. This gates the events the shell
    /// synthesizes on the seat, and a stick that started moving a pointer
    /// because Valve's client was running underneath would be aiming at a
    /// screen with nothing on it.
    fn has_application_window(&self) -> bool {
        self.lxb
            .space
            .elements()
            .any(|window| window_accepts_keyboard_focus(window) && !self.lxb.out_of_sight(window))
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
        // In the surface's own coordinates, as a pointer's motion is — and here
        // it matters for the whole gesture rather than one event: the seat keeps
        // the origin a finger went down at and measures every motion of that
        // slot against it, so a down delivered in the screen's space and a
        // motion in the window's would drag away from the finger.
        let (under, delivered) = match hit.as_ref() {
            Some(hit) => (
                Some((self.input_target_for_surface(&hit.surface), hit.origin)),
                hit.point,
            ),
            None => (None, location),
        };

        touch.down(
            self,
            under.clone(),
            &DownEvent {
                slot: event.slot(),
                location: delivered,
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
                // And a finger put down on an application takes that display,
                // exactly as a click does — the shell's own touch handler moves
                // control on a finger put down on one of its surfaces, and this
                // is the same rule for the surfaces it is behind.
                self.tell_shell_where_the_press_landed(&window, location);
            } else if hit.as_ref().is_some_and(|hit| {
                click_takes_keyboard_focus(self.layer_accepts_keyboard_focus(&hit.surface))
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
        let hit = self.surface_under(location);
        // The finger's own space again, and the seat measures this against the
        // origin the slot went down at — so both have to be in it. See
        // [`Self::on_touch_down`].
        let (under, delivered) = match hit.as_ref() {
            Some(hit) => (
                Some((self.input_target_for_surface(&hit.surface), hit.origin)),
                hit.point,
            ),
            None => (None, location),
        };
        touch.motion(
            self,
            under,
            &TouchMotionEvent {
                slot: event.slot(),
                location: delivered,
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
        let wanted = location;

        // Already inside an output: nothing to do.
        if self.lxb.space.output_under(location).next().is_some() {
            watch_a_clamped_pointer(&mut self.lxb, false, wanted, location);
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
        watch_a_clamped_pointer(&mut self.lxb, true, wanted, location);
    }

    /// Resolve the surface under a logical point, honouring layer ordering:
    /// overlay > top > windows > bottom > background.
    pub fn surface_under(&self, location: Point<f64, Logical>) -> Option<Hit> {
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
                    // The shell's own surfaces are never scaled — see
                    // [`crate::scale`] — so a hit on one is in the screen's own
                    // coordinates, which is what `Mapping::none` says.
                    .map(|(s, p)| Hit {
                        surface: s,
                        origin: p.to_f64() + layer_loc.to_f64() + output_loc,
                        point: location,
                        mapping: crate::scale::Mapping::none(),
                    })
            })
        };

        if let Some(hit) = layer_hit([WlrLayer::Overlay, WlrLayer::Top]) {
            return Some(hit);
        }

        if let Some((window, window_loc, mapping)) = self.window_under(location) {
            // Everything from here on is in the window's own coordinates,
            // which on a scaled application is not the screen's: the surface
            // tree was laid out at the size the client was configured at, so
            // the point has to be brought into that space before it is asked
            // which surface it landed on.
            let point = mapping.into_window(location);
            if let Some((s, p)) =
                window.surface_under(point - window_loc.to_f64(), WindowSurfaceType::ALL)
            {
                return Some(Hit {
                    surface: s,
                    origin: p.to_f64() + window_loc.to_f64(),
                    point,
                    mapping,
                });
            }
        }

        layer_hit([WlrLayer::Bottom, WlrLayer::Background])
    }

    /// The topmost window the pointer is actually over.
    ///
    /// `Space::element_under` with one thing taken out of it: a window the
    /// shell is keeping out of sight is not there to be pointed at, and the
    /// search carries on *underneath* it rather than stopping. Stopping would
    /// be the visible difference — a game with one of Valve's invisible
    /// dialogs over its middle would have a rectangle in it that swallowed
    /// every click.
    ///
    /// The geometry is smithay's own: a window is mapped by the frame the user
    /// thinks of as the window, and a client drawing its own decorations puts
    /// that frame inside a larger surface, so the surface starts before the
    /// mapped location does. That difference is what `render_location` is, and
    /// input has to subtract it exactly as drawing does.
    ///
    /// The third thing returned is the step between the screen's coordinates
    /// and this window's own, which is the identity except on an application
    /// drawing larger than life — see [`crate::scale`]. Everything after the
    /// point where it is applied is unchanged, because in the window's own
    /// space nothing about it has changed: it is the same surface tree at the
    /// same size, and only the coordinate arriving at it was somewhere else.
    fn window_under(
        &self,
        location: Point<f64, Logical>,
    ) -> Option<(Window, Point<i32, Logical>, crate::scale::Mapping)> {
        use smithay::desktop::space::SpaceElement;

        self.lxb
            .space
            .elements()
            .rev()
            .filter(|window| !self.lxb.out_of_sight(window))
            .find_map(|window| {
                let mapped = self.lxb.space.element_location(window)?;
                let render_location = mapped - window.geometry().loc;
                // Anchored at the window's mapped corner, which is where the
                // render anchors the growth it has to agree with: pixels and
                // presses have to come apart nowhere.
                let mapping = crate::scale::Mapping::of(
                    mapped.to_f64(),
                    crate::scale::window_scale(self.lxb.outputs.app_scale(), window),
                );
                let point = mapping.into_window(location);
                let mut bbox = window.bbox();
                bbox.loc += render_location;
                if !bbox.to_f64().contains(point) {
                    return None;
                }
                window
                    .is_in_input_region(&(point - render_location.to_f64()))
                    .then(|| (window.clone(), render_location, mapping))
            })
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
            self.log_keyboard_focus(target.as_ref());
            keyboard.set_focus(self, target, serial);
        }
    }

    /// Tell each application whether it is the one with the keyboard, and make
    /// sure it hears about it.
    ///
    /// An application that is not in front has to be told so. It is the only
    /// way it can know: a client cannot see the screen, and "am I the one the
    /// user is looking at" is not a question it can ask. A game that is never
    /// told goes on believing it is being played — which is exactly what it
    /// did. The user pressed the Home button, walked back to the start screen,
    /// and the game carried on: still drawing frames at whatever rate it
    /// pleased, and still playing its music, from behind a start screen that
    /// covered it completely.
    ///
    /// It looked like a Wayland problem and it was one, but not the client's.
    /// An X11 game *does* stop, because the one place focus is handed over in
    /// smithay's X11 path sets the X input focus as a side effect, and Wine
    /// turns the resulting `FocusOut` into everything a Windows program expects
    /// when it goes to the background. Nothing did the equivalent for an
    /// `xdg_toplevel`. Activation was set — [`smithay::desktop::Space`] sets it
    /// on every raise — but only ever into the *pending* state, and nothing
    /// sent the configure that carries pending state to the client, so the
    /// whole session ran with every toplevel believing it was activated.
    ///
    /// What decides it is being *on screen*, not holding the keyboard, and the
    /// difference is a game that never came back. Tying it to the keyboard put
    /// a Unity game to sleep the moment the guide borrowed the keys — which is
    /// right by the letter of xdg-shell and wrong here: the guide is glass, the
    /// game is still on the screen underneath it, and its cards in the overview
    /// are that very window drawing. Told it was deactivated, the game stopped
    /// its loop and sat in Wine's message wait at zero CPU with the correct X
    /// focus and `_NET_WM_STATE_FOCUSED` still on it, which from the outside
    /// looks exactly like a game that never got focus back. Measured on the
    /// real session: 10 CPU ticks per four seconds before, 0 after, and no
    /// pixel of it changed again.
    ///
    /// So both halves are here: the state on every window, and the configure
    /// that delivers it.
    ///
    /// Whether to send one is smithay's decision and not this function's, and
    /// that is the whole subtlety. The obvious version asks `set_activated`
    /// whether it changed anything and sends only then — and it misses exactly
    /// the case that matters. Raising a window already sets the pending state
    /// on every *other* window, silently, so by the time the keyboard has
    /// finished moving there is nothing left for this to change and the window
    /// that just lost the screen is never told. Verified: a client covered by a
    /// second one heard nothing at all until this asked unconditionally.
    /// `send_pending_configure` compares against what the client was actually
    /// last sent, which is the only comparison that answers the question.
    pub(crate) fn refresh_window_activation(&mut self) {
        // One answer per display: the application in front of it, and nothing
        // at all where the shell is painting over the whole thing.
        let fronts: Vec<Option<Window>> = self
            .lxb
            .space
            .outputs()
            .cloned()
            .collect::<Vec<_>>()
            .iter()
            .map(|output| crate::render::front_application_on_screen(&self.lxb, output))
            .collect();
        let windows: Vec<Window> = self.lxb.space.elements().cloned().collect();
        for window in windows {
            let in_front = fronts
                .iter()
                .flatten()
                .any(|front| front == &window || same_application(front, &window));
            window.set_activated(in_front);
            // X11 activation is a property, written by `set_activated` itself.
            // A toplevel's is a state on a configure it has not been sent yet.
            if let Some(toplevel) = window.toplevel() {
                toplevel.send_pending_configure();
            }
        }
        // And the one property an X client reads to answer the same question
        // for itself — see [`LxbState::name_the_active_x11_window`], which is
        // where the whole of why it matters is written down.
        self.name_the_active_x11_window(&fronts);
    }

    /// Tell the X server which window this session considers active.
    ///
    /// `_NET_WM_STATE_FOCUSED` on the window says it to whoever asks about that
    /// window; `_NET_ACTIVE_WINDOW` on the root says it about the session, and
    /// it is the one a great many clients actually read. Wine reads it, so
    /// every game under Proton does: a window that does not find itself named
    /// there decides it is in the background, throws away the pointer motion
    /// arriving at it and comes back only on a click, which is spent on the
    /// waking and never reaches the game. That is a game nobody can use without
    /// clicking twice, and it is what this session was doing.
    ///
    /// The window named is the one the *frame throttle* named, which is the
    /// same answer given to `set_activated` a few lines up: the application the
    /// user can see. Not the one holding the keyboard — the guide borrows that
    /// while the game is still on the screen under its glass, and a game told
    /// it had gone to the background there is a game that stops. Where the
    /// shell is painting over the display, or where what is in front is a
    /// Wayland window, no X11 window is active and the property says so.
    ///
    /// One display's answer for a property the X screen has only one of. Two
    /// games on two screens cannot both be the active window, so the first
    /// found wins, which is the display nearest the front of the layout.
    ///
    /// Written only when it changes: this is asked wherever activation is, and
    /// that is several times a second on a session that is merely animating.
    fn name_the_active_x11_window(&mut self, fronts: &[Option<Window>]) {
        let active = fronts
            .iter()
            .flatten()
            .find_map(|window| window.x11_surface())
            .map(|surface| surface.window_id())
            .unwrap_or(x11rb::NONE);
        if self.lxb.x11_active_window == Some(active) {
            return;
        }
        let Some(probe) = self.lxb.x11_focus_probe.as_ref() else {
            return;
        };
        match probe.set_active_window(active) {
            Ok(()) => {
                self.lxb.x11_active_window = Some(active);
                tracing::debug!(window = active, "said which X11 window is active");
            }
            Err(err) => tracing::warn!(
                window = active,
                %err,
                "could not say which X11 window is active"
            ),
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
        // And a volume key that was down goes with them, or a session the user
        // has switched away from carries on turning itself down.
        self.lxb.volume_key.interrupt();

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
        if let Some(hit) = self.surface_under(location) {
            let target = self.input_target_for_surface(&hit.surface);
            // Raise the owning window so click-to-focus also raises.
            if let Some(window) = self.window_for_input_target(&target) {
                let accepts_focus = window_accepts_keyboard_focus(&window);
                self.raise_window(&window, accepts_focus);
                if accepts_focus {
                    self.set_window_keyboard_focus(&window);
                }
                // The screen's own coordinate, not the window's: what this
                // answers is which *display* the hand landed on.
                self.tell_shell_where_the_press_landed(&window, location);
            } else if click_takes_keyboard_focus(self.layer_accepts_keyboard_focus(&hit.surface)) {
                self.set_keyboard_target(Some(target));
            }
        }
    }

    /// Tell the shell which display a press on an application landed on, so the
    /// display it is driving follows the user's hand there.
    ///
    /// Only for a press on a window, which is only ever an application: the
    /// shell draws itself in layer surfaces and is delivered every press made on
    /// one of them, so it moves control itself when a press lands on a display
    /// it was not driving. Repeating that here would move control on the
    /// compositor's word first, and the press the shell then read would be one
    /// on a display it was already driving — pressing whatever it landed on,
    /// where a press claiming the display is meant to do nothing else.
    ///
    /// Where its own surfaces are behind an application it hears nothing at all:
    /// the overlay hands the clicks through so the application stays usable with
    /// a mouse, which makes the press that says the user has moved the one press
    /// it can never see. Hence this.
    ///
    /// The display the window belongs to, rather than the one the pointer is
    /// over, on the same terms as [`Self::keyboard_focus_output`]: an application
    /// belongs to the display it was started on, and that is the display whose
    /// windows, foreground and menu the shell would be moving control to. They
    /// only differ for a window keeping geometry of its own — X11 chrome — and
    /// then the display it hangs off the edge of is not the one it is on.
    fn tell_shell_where_the_press_landed(&mut self, window: &Window, at: Point<f64, Logical>) {
        let output = self
            .lxb
            .outputs
            .window_display(&self.lxb.space, window)
            .or_else(|| self.lxb.outputs.output_at(&self.lxb.space, at));
        if let Some(output) = output {
            self.lxb.shell_control.send_output_pressed(&output);
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
            .find(|window| window_accepts_keyboard_focus(window) && !self.lxb.out_of_sight(window))
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
            .find(|window| window_accepts_keyboard_focus(window) && !self.lxb.out_of_sight(window))
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
            self.cursor_moved();

            // Keep the seat's pointer focus in sync with the compositor-side
            // warp. Otherwise a button pressed before physical mouse motion
            // would still be delivered to the old output's surface.
            let location = self.lxb.pointer_location;
            let hit = self.surface_under(location);
            let (under, delivered) = match hit.as_ref() {
                Some(hit) => (
                    Some((self.input_target_for_surface(&hit.surface), hit.origin)),
                    hit.point,
                ),
                None => (None, location),
            };
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
                        location: delivered,
                        serial: SERIAL_COUNTER.next_serial(),
                        time,
                    },
                );
                self.activate_constraint_at(&pointer, hit.as_ref());
                pointer.frame(self);
            }
        }

        // Focus the topmost window on that output.
        let window = self
            .lxb
            .space
            .elements_for_output(target)
            .rev()
            .find(|window| window_accepts_keyboard_focus(window) && !self.lxb.out_of_sight(window))
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
            .filter(|window| {
                window_accepts_keyboard_focus(window) && !self.lxb.out_of_sight(window)
            })
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
    old_hit: Option<&Hit>,
    new_hit: Option<&Hit>,
) -> bool {
    let Some(ActivePointerConstraint::Confined(region)) = constraint else {
        return true;
    };
    let (Some(old), Some(new)) = (old_hit, new_hit) else {
        return false;
    };
    old.surface == new.surface
        && region
            .as_ref()
            // The client's region, so the client's own coordinates: a [`Hit`]
            // has already put the point into them.
            .map(|region| region.contains(new.local().to_i32_round()))
            .unwrap_or(true)
}

/// Something that happens to some motion events and not to others, counted so
/// that the log can say it once a second rather than once an event.
///
/// Both of the things below are edges in name only, and treating them as edges
/// is what buried a session's log in them: a pointer resting against the bottom
/// of a display is sent past it by one event and back inside by the next, all
/// day, at the rate the mouse reports. What the two of them can honestly say is
/// a rate — how often the pointer is being sent somewhere it cannot go — and
/// that a whole second has gone by without it happening again.
#[derive(Debug, Clone, Copy)]
pub struct Repeatedly {
    /// When the last line about it was written.
    said: std::time::Instant,
    /// When it last actually happened, which is what says it has stopped.
    last: std::time::Instant,
    /// How many times since that line.
    times: u32,
}

/// How long a run of these goes unmentioned, and how long the quiet has to be
/// before it counts as over.
const REPEATEDLY: std::time::Duration = std::time::Duration::from_secs(1);

impl Repeatedly {
    /// Note one more, answering with how many have happened since the last
    /// line when it is time to write another — and `None` while it is not.
    fn again(slot: &mut Option<Self>, now: std::time::Instant) -> Option<u32> {
        let Some(state) = slot else {
            // The first is worth a line on its own, and immediately: it is the
            // one that says which of these is happening at all.
            *slot = Some(Self {
                said: now,
                last: now,
                times: 0,
            });
            return Some(0);
        };
        state.last = now;
        state.times += 1;
        if now.duration_since(state.said) < REPEATEDLY {
            return None;
        }
        let times = state.times;
        state.said = now;
        state.times = 0;
        Some(times)
    }

    /// Note that it did not happen this time, answering whether that is the
    /// end of a run — a whole second of the pointer moving freely, rather than
    /// the gap between two events at the edge of a screen.
    fn stopped(slot: &mut Option<Self>, now: std::time::Instant) -> bool {
        let Some(state) = slot else {
            return false;
        };
        if now.duration_since(state.last) < REPEATEDLY {
            return false;
        }
        *slot = None;
        true
    }
}

/// Say so while a confinement is refusing the pointer's motion, and again once
/// it has stopped.
///
/// A confined pointer that will not move looks exactly like a compositor that
/// has stopped reading the mouse, and from the log the two were
/// indistinguishable. Reported by the second rather than by the event: this is
/// asked several hundred times a second, and a pointer held against the edge of
/// its own region is refused and let go alternately for as long as the hand
/// keeps pushing. See [`Repeatedly`].
fn watch_a_refused_pointer(lxb: &mut crate::state::Lxb, refused: bool, at: Point<f64, Logical>) {
    let now = std::time::Instant::now();
    if !refused {
        if Repeatedly::stopped(&mut lxb.pointer_refused, now) {
            tracing::info!(?at, "the pointer is moving again");
        }
        return;
    }
    if let Some(times) = Repeatedly::again(&mut lxb.pointer_refused, now) {
        tracing::info!(
            ?at,
            in_the_last_second = times,
            "a confined pointer is refusing to move"
        );
    }
}

/// Say so while the pointer is being held on screen, and again once it has
/// stopped.
///
/// The other way a cursor that will not move looks from the outside, and the
/// one that means something is driving it somewhere there is no display: what
/// the user sees is an arrow welded to a corner, because a corner is the
/// nearest place to wherever it was sent. Counted rather than edge-triggered,
/// for the reason [`watch_a_refused_pointer`] gives — and here the count is
/// also the difference between the two things this can be. A hand resting the
/// pointer against the bottom of a screen crosses the edge a few dozen times a
/// second and lands a pixel outside; a client driving it off the layout does it
/// once and lands half a million pixels away.
fn watch_a_clamped_pointer(
    lxb: &mut crate::state::Lxb,
    clamped: bool,
    wanted: Point<f64, Logical>,
    held_at: Point<f64, Logical>,
) {
    let now = std::time::Instant::now();
    if !clamped {
        if Repeatedly::stopped(&mut lxb.pointer_clamped, now) {
            tracing::info!(at = ?held_at, "the pointer is back on a display");
        }
        return;
    }
    if let Some(times) = Repeatedly::again(&mut lxb.pointer_clamped, now) {
        tracing::info!(
            ?wanted,
            ?held_at,
            in_the_last_second = times,
            "the pointer is being held on screen"
        );
    }
}

/// Say so when a client puts the pointer somewhere itself.
///
/// This is `set_cursor_position_hint`: a place a client may name only while it
/// holds a pointer lock, and which is honoured once that lock is let go. One
/// of them is an application restoring its own cursor, which is the feature.
/// One of them *per frame* is an application the user cannot move the mouse
/// away from — so the number of them is the whole point, and this is rate
/// limited rather than edge-triggered.
fn watch_a_client_placing_the_pointer(
    lxb: &mut crate::state::Lxb,
    from: Point<f64, Logical>,
    to: Point<f64, Logical>,
) {
    const EVERY: std::time::Duration = std::time::Duration::from_secs(1);
    let now = std::time::Instant::now();
    let Some((since, count)) = lxb.pointer_hints else {
        // The first one is worth a line on its own: an application that places
        // the pointer once is not the same thing as one doing it constantly,
        // and waiting a second to say so would lose the difference.
        lxb.pointer_hints = Some((now, 0));
        tracing::info!(?from, ?to, "a client placed the pointer itself");
        return;
    };
    if now.duration_since(since) < EVERY {
        lxb.pointer_hints = Some((since, count + 1));
        return;
    }
    lxb.pointer_hints = Some((now, 0));
    tracing::info!(
        ?from,
        ?to,
        in_the_last_second = count + 1,
        "a client is placing the pointer itself"
    );
}

/// Whether a mapped desktop window is an application target rather than
/// non-interactive X11 chrome. Override-redirect games with normal window
/// types remain focusable; menus, notifications and similar transient UI do
/// not black-hole the seat.
///
/// `WM_HINTS input=false` alone is deliberately not rejected: ICCCM globally
/// active clients combine that hint with `WM_TAKE_FOCUS`, and Smithay's X11
/// keyboard target implements the required protocol handshake.
///
/// A window that has said *nothing whatever* about itself is chrome too, for
/// the reason given at [`x11_window_says_nothing`].
pub(crate) fn window_is_x11_chrome(window: &Window) -> bool {
    let Some(surface) = window.x11_surface() else {
        return false;
    };

    if x11_window_says_nothing(surface) {
        return true;
    }

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

/// Whether an X11 window has told us nothing at all about itself: no class, no
/// instance, no title, no window type, no `WM_HINTS`, no `_NET_WM_PID`.
///
/// Wine gives every process it starts a handful of X11 windows that are not
/// windows in any sense the user would recognise — the cursor clipping window,
/// a per-thread IME window, and one bare toplevel per short-lived helper
/// process. A game under Proton is surrounded by them: Valve's client runs
/// `d3ddriverquery64.exe` over and over while a game is up, and each run maps
/// one of these and takes it away again a few milliseconds later.
///
/// Treated as an application, one of those does three things to the game it
/// appears over, and all three were reported as the game's own fault. It is
/// tiled across the whole display, so it covers the picture. It is the topmost
/// window that takes focus, so it becomes the application in front — and the
/// game behind it, being another process, is starved of frame callbacks and
/// stops dead. And it is announced to the shell as a display-filling window,
/// so when it goes the shell reads an application walking out and brings the
/// start screen forward over a game that is still running.
///
/// The test is deliberately unanimous rather than a guess at a shape: any one
/// of those six properties makes this an application window and this returns
/// false. A game that sets nothing but a title — and there is one in this
/// session's own logs — is still a game. Nothing that draws for a user arrives
/// with all six missing, and everything Wine leaves lying about does.
///
/// It is also not a one-time judgement. A client is free to create a window,
/// map it, and only then say what it is; [`XwmHandler::property_notify`] asks
/// again whenever one of these properties lands, and a window that names
/// itself late is promoted to an application there.
///
/// [`XwmHandler::property_notify`]: smithay::xwayland::XwmHandler::property_notify
pub(crate) fn x11_window_says_nothing(surface: &X11Surface) -> bool {
    says_nothing(
        &surface.class(),
        &surface.instance(),
        &surface.title(),
        surface.window_type().is_some(),
        surface.hints().is_some(),
        surface.pid().is_some(),
    )
}

/// The same question of the six answers themselves, so it can be asked without
/// an X server.
fn says_nothing(
    class: &str,
    instance: &str,
    title: &str,
    has_window_type: bool,
    has_hints: bool,
    has_pid: bool,
) -> bool {
    class.trim().is_empty()
        && instance.trim().is_empty()
        && title.trim().is_empty()
        && !has_window_type
        && !has_hints
        && !has_pid
}

pub(crate) fn window_accepts_keyboard_focus(window: &Window) -> bool {
    // A window that has already gone is still in the space for a moment, and
    // handing it the keyboard is not harmless: Smithay drops the X input focus
    // on the way out of whatever held it before, then addresses a window the X
    // server has destroyed and gets `BadWindow` for it. The keyboard lands
    // nowhere, the next tick puts it back, and the window that is really on
    // screen is left taking focus in and focus out several times a second.
    window.alive() && !window_is_x11_chrome(window) && x11_window_accepts_input(window)
}

/// Record who has just been handed the keyboard.
///
/// Every hand-over, not only the X11 ones. "Which of the shell and the game has
/// the keys" is the first question asked of a session where a button did
/// nothing, and the shell's half of the answer used to be a debug line — so a
/// journal kept at the level a session actually runs at showed the keyboard
/// arriving at a game and never leaving it, whatever had happened in between.
/// There is one of these per guide opened or closed, which is not a rate worth
/// hiding anything at.
///
/// The X11 case carries the most, because an X11 window is focused twice over:
/// the surface takes the Wayland keyboard, and Smithay sets the X input focus
/// underneath as a side effect, following the window's own `WM_HINTS`. Which of
/// those it asked for decides whether the focus lands at all, so the hint is
/// logged beside the window rather than left to be guessed: a window mapped
/// with `input = Some(false)` and no `WM_TAKE_FOCUS` is one the X server will
/// never give the keyboard to, however plainly it is the thing on screen.
///
/// A client mapping several windows under one class is the case that decides
/// the shape of it. The name alone cannot tell them apart, so the window id is
/// what ties this to [`LxbState::check_xwayland_focus`] and to the ids the map
/// logs.
impl LxbState {
    fn log_keyboard_focus(&self, target: Option<&KeyboardFocusTarget>) {
        match target {
            Some(KeyboardFocusTarget::X11(surface)) => tracing::info!(
                window = surface.window_id(),
                title = surface.title(),
                class = surface.class(),
                wants_input = ?surface.hints().and_then(|hints| hints.input),
                "the keyboard went to an X11 window"
            ),
            // The shell is worth telling apart from any other client, and it is
            // the one a compositor can always name: this is the guide or the
            // board taking the keys off whatever is on screen, which is the
            // event on one side of every "the button did nothing".
            Some(KeyboardFocusTarget::Wayland(surface))
                if smithay::reexports::wayland_server::Resource::client(surface)
                    .is_some_and(|client| self.lxb.shell_control.is_shell_client(&client)) =>
            {
                tracing::info!("the keyboard went to the session shell")
            }
            Some(KeyboardFocusTarget::Wayland(surface)) => {
                let window = self.lxb.window_for_surface(surface);
                tracing::info!(
                    app_id = window
                        .as_ref()
                        .map(crate::shell_control::window_app_id)
                        .unwrap_or_default(),
                    title = window
                        .as_ref()
                        .map(crate::shell_control::window_title)
                        .unwrap_or_default(),
                    "the keyboard went to a Wayland window"
                )
            }
            // Which is a session with nowhere for a key to land, and says so:
            // it is what the user is looking at when nothing they press does
            // anything at all.
            None => tracing::info!("the keyboard went nowhere"),
        }
    }
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

/// Whether a movement is one the client hears on the relative stream too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RelativeStream {
    /// A real movement — a mouse, or the shell's stick. The client is told
    /// both ways, which is what `wp_relative_pointer_v1` is for: a game reads
    /// its camera off the relative stream while its cursor moves.
    Send,
    /// Catching up with a pointer something else has already moved.
    ///
    /// The client moved it *itself*, so it needs no telling — and telling it
    /// anyway is a feedback loop rather than a redundancy. XWayland moves its
    /// own pointer by whatever relative motion it is sent, so a correction of
    /// `theirs - ours` sent this way lands twice: ours arrives at theirs, and
    /// theirs moves the same distance again. The difference between the two
    /// is therefore exactly what it was before the correction, and the next
    /// poll sends it again. What the user sees is the cursor tearing off at a
    /// constant speed — measured at a quarter of the screen every 8ms — until
    /// it reaches the edge of the layout and parks in whichever corner the
    /// first difference happened to point at, unmovable, because no hand on a
    /// mouse can outrun it.
    Withhold,
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
    use super::says_nothing;

    /// The windows Wine leaves lying around every game under Proton: no class,
    /// no instance, no title, no type, no hints, no pid. Tiled over the
    /// display and given the keyboard, one of these covers the game, starves
    /// it of frames, and — when the helper process that owns it exits a few
    /// milliseconds later — tells the shell an application has walked out.
    #[test]
    fn a_window_that_says_nothing_is_not_an_application() {
        assert!(says_nothing("", "", "", false, false, false));
        // Whitespace is not a name either.
        assert!(says_nothing("  ", "", " ", false, false, false));
    }

    /// And any one word about itself makes it one. Unanimity is the point: the
    /// cost of getting this wrong is a game treated as furniture, which is a
    /// game that never appears, so every doubt is resolved towards the window
    /// being real.
    #[test]
    fn one_word_about_itself_is_enough_to_be_an_application() {
        assert!(!says_nothing(
            "steam_app_3812600",
            "",
            "",
            false,
            false,
            false
        ));
        assert!(!says_nothing("", "restory", "", false, false, false));
        // A game in this session's own logs mapped with a title and no class
        // at all.
        assert!(!says_nothing("", "", "ScannerSombre", false, false, false));
        assert!(!says_nothing("", "", "", true, false, false));
        assert!(!says_nothing("", "", "", false, true, false));
        assert!(!says_nothing("", "", "", false, false, true));
    }

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
        assert_eq!(
            Action::parse("volume-up"),
            Some(Action::Volume(VolumeChange::Up))
        );
        assert_eq!(
            Action::parse("volume-down"),
            Some(Action::Volume(VolumeChange::Down))
        );
        for spelling in ["volume-mute", "mute"] {
            assert_eq!(
                Action::parse(spelling),
                Some(Action::Volume(VolumeChange::Mute))
            );
        }
        assert_eq!(Action::parse("nonsense"), None);
    }

    /// The keys with a speaker printed on them, which a session with no
    /// desktop behind it has nothing else to answer: no settings daemon, no
    /// applet, and — while a game holds the keyboard — no shell either.
    #[test]
    fn the_volume_keys_are_bound_out_of_the_box() {
        let bindings = KeyBindings::from_config(&Config::default());
        let mods = ModifiersState::default();
        for (key, action) in [
            (keysyms::KEY_XF86AudioRaiseVolume, VolumeChange::Up),
            (keysyms::KEY_XF86AudioLowerVolume, VolumeChange::Down),
            (keysyms::KEY_XF86AudioMute, VolumeChange::Mute),
        ] {
            assert_eq!(
                bindings.lookup(&mods, [Keysym::from(key)]),
                Some(Action::Volume(action)),
                "{action:?}"
            );
        }
    }

    /// And they answer whatever is held with them, for the reason the
    /// screenshot key does: they are keys with one job printed on them,
    /// reached through Fn on a laptop and through a media row on a keyboard,
    /// and no two of those agree about what else is down at the time.
    #[test]
    fn the_volume_keys_do_not_care_what_is_held_with_them() {
        let bindings = KeyBindings::from_config(&Config::default());
        let up = [Keysym::from(keysyms::KEY_XF86AudioRaiseVolume)];
        for held in [
            ModifiersState {
                shift: true,
                ..Default::default()
            },
            ModifiersState {
                ctrl: true,
                alt: true,
                ..Default::default()
            },
            ModifiersState {
                logo: true,
                ..Default::default()
            },
        ] {
            assert_eq!(
                bindings.lookup(&held, up),
                Some(Action::Volume(VolumeChange::Up))
            );
        }
    }

    /// A held volume key steps until it is let go of, and the release that
    /// stops it is its own. Turning up and then down without letting go of the
    /// first hands the repeat over, and the release of the key that lost it
    /// must not stop the one now running.
    #[test]
    fn only_the_key_that_owns_the_repeat_can_end_it() {
        let mut held = VolumeKey::default();
        let up = Keycode::new(123);
        let down = Keycode::new(124);

        let first = held.pressed(up);
        assert!(held.wants(first));

        let second = held.pressed(down);
        assert!(held.wants(second));
        assert!(
            !held.wants(first),
            "the timer for the first key is not wanted once the second has it"
        );

        held.released(up);
        assert!(held.wants(second), "the key still down keeps stepping");
        held.released(down);
        assert!(!held.wants(second));
    }

    /// The session losing the keys — a VT switch, focus taken away — stops it
    /// too, or a session nobody is looking at goes on turning itself down.
    #[test]
    fn a_session_that_loses_the_keys_stops_stepping() {
        let mut held = VolumeKey::default();
        let booked = held.pressed(Keycode::new(123));
        assert!(held.wants(booked));
        held.interrupt();
        assert!(!held.wants(booked));
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
