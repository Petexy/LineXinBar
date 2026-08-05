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
use crate::state::LinboardState;
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
        self.ctrl == mods.ctrl
            && self.alt == mods.alt
            && self.logo == mods.logo
            // Shift changes the modified keysym, so compare the raw one and
            // require an exact shift match.
            && self.shift == mods.shift
            && self.keysym == fold_case(keysym)
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

/// The compositor's keybinding table.
#[derive(Debug, Default)]
pub struct KeyBindings {
    bindings: Vec<(KeyPattern, Action)>,
}

impl KeyBindings {
    pub fn from_config(config: &Config) -> Self {
        let mut bindings: Vec<(KeyPattern, Action)> = Vec::new();

        // Built-in defaults. Explicit config entries override these.
        let defaults = [
            ("Ctrl+Alt+BackSpace", Action::Quit),
            ("Super+Q", Action::CloseWindow),
            // The guide overlay is the way out of a running application, so it
            // gets two spellings: one for keyboards without a Home key, and
            // the media key that handhelds and remotes actually send.
            ("Super+G", Action::Guide),
            ("Super+Home", Action::Guide),
            ("XF86HomePage", Action::Guide),
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
                (Some(pattern), Some(action)) => {
                    bindings.retain(|(p, _)| *p != pattern);
                    bindings.push((pattern, action));
                }
                (None, _) => tracing::warn!(binding = raw, "ignoring unparseable keybinding"),
                (_, None) => tracing::warn!(action = action, "ignoring unknown action"),
            }
        }

        Self { bindings }
    }

    fn lookup(&self, mods: &ModifiersState, keysym: Keysym) -> Option<Action> {
        self.bindings
            .iter()
            .find(|(pattern, _)| pattern.matches(mods, keysym))
            .map(|(_, action)| action.clone())
    }
}

impl LinboardState {
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
                if let Some(touch) = self.linboard.seat.get_touch() {
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
        if let Some(touch) = self.linboard.seat.get_touch() {
            touch.frame(self);
        }
    }

    fn on_keyboard<B: InputBackend>(&mut self, event: B::KeyboardKeyEvent) {
        let serial = SERIAL_COUNTER.next_serial();
        let time = Event::time_msec(&event);
        let keycode = event.key_code();
        let state = event.state();

        let Some(keyboard) = self.linboard.seat.get_keyboard() else {
            return;
        };

        let action = keyboard.input(self, keycode, state, serial, time, |state, mods, handle| {
            // Match the modified symbol first, then the raw one, so that both
            // `Super+Q` and shift-rewritten combos like `Super+Shift+Right`
            // resolve to the same binding.
            let candidates = std::iter::once(handle.modified_sym()).chain(handle.raw_syms());
            for sym in candidates {
                if let Some(action) = state.linboard.keybindings.lookup(mods, sym) {
                    return FilterResult::Intercept(action);
                }
            }
            FilterResult::Forward
        });

        // Actions fire on press only; the matching release is swallowed too,
        // which is what clients expect from a grabbed binding.
        if let Some(action) = action {
            if state == KeyState::Pressed {
                self.run_action(action);
            }
        }
    }

    fn run_action(&mut self, action: Action) {
        tracing::debug!(?action, "running action");
        match action {
            Action::Quit => {
                self.linboard.running = false;
                self.linboard.loop_signal.stop();
            }
            Action::CloseWindow => {
                if let Some(window) = self.focused_window() {
                    self.request_window_close(&window);
                }
            }
            Action::Spawn(cmd) => self.linboard.spawn(&cmd),
            Action::SwitchVt(vt) => self.backend.switch_vt(vt),
            Action::FocusNextOutput => self.cycle_output(1),
            Action::FocusPrevOutput => self.cycle_output(-1),
            Action::MoveWindowToNextOutput => self.move_window_to_next_output(),
            Action::CycleWindow => self.cycle_window(),
            Action::Guide => self.open_guide(),
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

    fn on_pointer_motion<B: InputBackend>(&mut self, event: B::PointerMotionEvent) {
        let serial = SERIAL_COUNTER.next_serial();
        let delta = event.delta();
        let Some(pointer) = self.linboard.seat.get_pointer() else {
            return;
        };
        self.apply_released_pointer_hint(&pointer);

        let old_location = self.linboard.pointer_location;
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
                delta_unaccel: event.delta_unaccel(),
                utime: event.time(),
            },
        );

        if matches!(constraint, Some(ActivePointerConstraint::Locked)) {
            pointer.frame(self);
            return;
        }

        self.linboard.pointer_location = old_location + delta;
        self.clamp_pointer();
        let location = self.linboard.pointer_location;
        let hit = self.surface_under(location);

        if !confined_motion_is_valid(
            constraint.as_ref(),
            old_hit.as_ref(),
            hit.as_ref(),
            location,
        ) {
            self.linboard.pointer_location = old_location;
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
                time: event.time_msec(),
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

        // Absolute devices report in 0.0..=1.0 of one output's area.
        let output = target_output.cloned().or_else(|| {
            self.linboard
                .outputs
                .output_at(&self.linboard.space, self.linboard.pointer_location)
                .or_else(|| self.linboard.space.outputs().next().cloned())
        });
        let Some(output) = output else { return };
        let Some(geometry) = self.linboard.space.output_geometry(&output) else {
            return;
        };

        let Some(pointer) = self.linboard.seat.get_pointer() else {
            return;
        };
        self.apply_released_pointer_hint(&pointer);

        let reported_location =
            geometry.loc.to_f64() + absolute_position::<B, _>(&event, geometry.size, source_size);
        let nested = !matches!(self.backend, crate::backend::Backend::Udev(_));
        let nested_delta = if nested {
            self.linboard
                .nested_host_pointer_location
                .replace(reported_location)
                .map(|previous| reported_location - previous)
        } else {
            None
        };

        let old_location = self.linboard.pointer_location;
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
        self.linboard.pointer_location = if nested {
            nested_delta
                .map(|delta| old_location + delta)
                .unwrap_or(reported_location)
        } else {
            reported_location
        };
        self.clamp_pointer();
        let location = self.linboard.pointer_location;
        let hit = self.surface_under(location);
        if !confined_motion_is_valid(
            constraint.as_ref(),
            old_hit.as_ref(),
            hit.as_ref(),
            location,
        ) {
            self.linboard.pointer_location = old_location;
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
        let Some((surface, hint)) = self.linboard.pointer_position_hint.clone() else {
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
            self.linboard.pointer_position_hint = None;
            return;
        }

        let Some((current, origin)) = self.surface_under(self.linboard.pointer_location) else {
            self.linboard.pointer_position_hint = None;
            return;
        };
        if current != surface {
            self.linboard.pointer_position_hint = None;
            return;
        }

        self.linboard.pointer_location = origin + hint;
        self.clamp_pointer();
        self.linboard.pointer_position_hint = None;
    }

    fn on_pointer_button<B: InputBackend>(&mut self, event: B::PointerButtonEvent) {
        let serial = SERIAL_COUNTER.next_serial();
        let button = event.button_code();
        let state = event.state();

        if state == ButtonState::Pressed {
            self.focus_under_pointer(serial);
        }

        if let Some(pointer) = self.linboard.seat.get_pointer() {
            pointer.button(
                self,
                &ButtonEvent {
                    button,
                    state,
                    serial,
                    time: event.time_msec(),
                },
            );
            pointer.frame(self);
        }
    }

    fn on_pointer_axis<B: InputBackend>(&mut self, event: B::PointerAxisEvent) {
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

        if let Some(pointer) = self.linboard.seat.get_pointer() {
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
        let Some(touch) = self.linboard.seat.get_touch() else {
            return;
        };
        let Some(output) = target_output
            .cloned()
            .or_else(|| self.linboard.space.outputs().next().cloned())
        else {
            return;
        };
        let Some(geometry) = self.linboard.space.output_geometry(&output) else {
            return;
        };

        let location =
            geometry.loc.to_f64() + absolute_position::<B, _>(&event, geometry.size, source_size);
        let serial = SERIAL_COUNTER.next_serial();
        let under = self
            .surface_under(location)
            .map(|(surface, origin)| (self.input_target_for_surface(&surface), origin));

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
        // must not steal keyboard/controller focus from the application.
        if let Some((target, _)) = under {
            if let Some(window) = self.window_for_input_target(&target) {
                if window_accepts_keyboard_focus(&window) {
                    self.raise_window(&window, true);
                    self.set_window_keyboard_focus(&window);
                }
            } else {
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
        let Some(touch) = self.linboard.seat.get_touch() else {
            return;
        };
        let Some(output) = target_output
            .cloned()
            .or_else(|| self.linboard.space.outputs().next().cloned())
        else {
            return;
        };
        let Some(geometry) = self.linboard.space.output_geometry(&output) else {
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
        let Some(touch) = self.linboard.seat.get_touch() else {
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
        let mut location = self.linboard.pointer_location;

        // Already inside an output: nothing to do.
        if self.linboard.space.output_under(location).next().is_some() {
            return;
        }

        // Otherwise snap to the closest point of the nearest output.
        let mut best: Option<(f64, Point<f64, Logical>)> = None;
        for output in self.linboard.space.outputs() {
            let Some(geo) = self.linboard.space.output_geometry(output) else {
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
        self.linboard.pointer_location = location;
    }

    /// Resolve the surface under a logical point, honouring layer ordering:
    /// overlay > top > windows > bottom > background.
    pub fn surface_under(
        &self,
        location: Point<f64, Logical>,
    ) -> Option<(WlSurface, Point<f64, Logical>)> {
        let output = self
            .linboard
            .outputs
            .output_at(&self.linboard.space, location)?;
        let output_geo = self.linboard.space.output_geometry(&output)?;
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

        if let Some((window, window_loc)) = self.linboard.space.element_under(location) {
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
        let keyboard = self.linboard.seat.get_keyboard()?;
        let focus = keyboard.current_focus()?;
        self.window_for_input_target(&focus)
    }

    fn window_for_input_target(&self, target: &KeyboardFocusTarget) -> Option<Window> {
        match target {
            KeyboardFocusTarget::Wayland(surface) => self.linboard.window_for_surface(surface),
            KeyboardFocusTarget::X11(surface) => self
                .linboard
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
        self.linboard
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
        let target = if !self.linboard.keyboard_focus_enabled {
            None
        } else {
            match &self.linboard.exclusive_keyboard_focus {
                Some(exclusive) => Some(KeyboardFocusTarget::Wayland(exclusive.clone())),
                None => target,
            }
        };
        if let Some(keyboard) = self.linboard.seat.get_keyboard() {
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
    /// Clearing focus on deactivation prevents a client inside Linboard from
    /// remaining logically active while the user is interacting with the host
    /// desktop. Releasing the internal key state also avoids stuck modifiers:
    /// winit deliberately filters the synthetic releases it receives on focus
    /// loss.
    pub fn nested_keyboard_focus_changed(&mut self, focused: bool, output: Option<&Output>) {
        self.linboard.keyboard_focus_enabled = focused;
        self.linboard.nested_host_pointer_location = None;

        if !focused {
            self.set_keyboard_focus(None);
            self.release_pressed_keys();
            if let Some(touch) = self.linboard.seat.get_touch() {
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
        let Some(keyboard) = self.linboard.seat.get_keyboard() else {
            return;
        };
        let pressed: Vec<_> = keyboard.pressed_keys().into_iter().collect();
        let time = self
            .linboard
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
        if !self.linboard.keyboard_focus_enabled {
            return false;
        }

        let Some(keyboard) = self.linboard.seat.get_keyboard() else {
            return false;
        };
        let Some(focus) = keyboard.current_focus() else {
            return true;
        };

        if let Some(exclusive) = &self.linboard.exclusive_keyboard_focus {
            return focus.wl_surface().as_deref() != Some(exclusive);
        }
        let window_is_mapped = match &focus {
            KeyboardFocusTarget::Wayland(surface) => {
                self.linboard.window_for_surface(surface).is_some()
            }
            KeyboardFocusTarget::X11(surface) => self.linboard.space.elements().any(|window| {
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

        for output in self.linboard.space.outputs() {
            let map = layer_map_for_output(output);
            if let Some(layer) = map.layer_for_surface(&focus_surface, WindowSurfaceType::ALL) {
                return !layer.can_receive_keyboard_focus();
            }
        }

        true
    }

    fn focus_under_pointer(&mut self, _serial: smithay::utils::Serial) {
        let location = self.linboard.pointer_location;
        if let Some((surface, _)) = self.surface_under(location) {
            let target = self.input_target_for_surface(&surface);
            // Raise the owning window so click-to-focus also raises.
            if let Some(window) = self.window_for_input_target(&target) {
                let accepts_focus = window_accepts_keyboard_focus(&window);
                self.raise_window(&window, accepts_focus);
                if accepts_focus {
                    self.set_window_keyboard_focus(&window);
                }
            } else {
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
        if let Some(exclusive) = self.linboard.exclusive_keyboard_focus.clone() {
            self.set_keyboard_focus(Some(exclusive));
            return;
        }

        let topmost = self
            .linboard
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
        if let Some(exclusive) = self.linboard.exclusive_keyboard_focus.clone() {
            self.set_keyboard_focus(Some(exclusive));
            return;
        }

        let topmost = self
            .linboard
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
        let keyboard = self.linboard.seat.get_keyboard()?;
        let surface = keyboard.current_focus()?.wl_surface()?.into_owned();

        if let Some(window) = self.linboard.window_for_surface(&surface) {
            if let Some(output) = self.linboard.space.outputs_for_element(&window).first() {
                return Some(output.clone());
            }
        }

        self.linboard
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
        for output in self.linboard.space.outputs() {
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
        let outputs: Vec<_> = self.linboard.space.outputs().cloned().collect();
        if outputs.len() < 2 {
            return;
        }
        let current = self
            .linboard
            .outputs
            .output_at(&self.linboard.space, self.linboard.pointer_location);
        let index = current
            .and_then(|c| outputs.iter().position(|o| *o == c))
            .unwrap_or(0) as i32;
        let next = (index + direction).rem_euclid(outputs.len() as i32) as usize;
        let target = &outputs[next];

        // Warp the pointer to the centre of the target output.
        if let Some(geo) = self.linboard.space.output_geometry(target) {
            self.linboard.pointer_location = Point::from((
                geo.loc.x as f64 + geo.size.w as f64 / 2.0,
                geo.loc.y as f64 + geo.size.h as f64 / 2.0,
            ));

            // Keep the seat's pointer focus in sync with the compositor-side
            // warp. Otherwise a button pressed before physical mouse motion
            // would still be delivered to the old output's surface.
            let location = self.linboard.pointer_location;
            let hit = self.surface_under(location);
            let under = hit
                .as_ref()
                .map(|(surface, origin)| (self.input_target_for_surface(surface), *origin));
            let time = self
                .linboard
                .start_time
                .elapsed()
                .as_millis()
                .min(u32::MAX as u128) as u32;
            if let Some(pointer) = self.linboard.seat.get_pointer() {
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
            .linboard
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
        let outputs: Vec<_> = self.linboard.space.outputs().cloned().collect();
        if outputs.len() < 2 {
            return;
        }
        let current = self
            .linboard
            .space
            .outputs_for_element(&window)
            .first()
            .cloned();
        let index = current
            .and_then(|c| outputs.iter().position(|o| *o == c))
            .unwrap_or(0);
        let target = &outputs[(index + 1) % outputs.len()];

        if let Some(geo) = self.linboard.space.output_geometry(target) {
            self.linboard
                .space
                .map_element(window.clone(), geo.loc, true);
            self.sync_x11_raise(&window);
            self.linboard
                .outputs
                .tile_window_on_output(&mut self.linboard.space, &window, target);
        }
    }

    fn cycle_window(&mut self) {
        let Some(output) = self
            .linboard
            .outputs
            .output_at(&self.linboard.space, self.linboard.pointer_location)
        else {
            return;
        };
        let windows: Vec<_> = self
            .linboard
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
    pointer: &smithay::input::pointer::PointerHandle<LinboardState>,
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

#[cfg(test)]
mod tests {
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

    #[test]
    fn maps_rectangular_nested_touch_coordinates_per_axis() {
        // Regresses Smithay 0.7's winit touch normalisation bug, where Y is
        // divided by the host window width. Raw window coordinates must use
        // the matching source extent for each axis.
        assert_eq!(map_window_coordinate(480.0, 960, 1920), 960.0);
        assert_eq!(map_window_coordinate(300.0, 600, 1200), 600.0);
    }
}
