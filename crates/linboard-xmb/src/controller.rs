//! Game-controller input for the shell.
//!
//! Wayland deliberately has no gamepad protocol.  A shell therefore has to
//! read controllers separately from its Wayland keyboard.  GilRs gives us
//! hot-plugging and the same SDL-compatible button mapping Steam exposes via
//! `SDL_GAMECONTROLLERCONFIG`, including the Steam Deck's built-in controls.

use std::time::Duration;

use gilrs::{Axis, Button, EventType, Gilrs, GilrsBuilder};

use crate::model::Action;

/// Controller state is sampled often enough that input never feels tied to a
/// 30/60 Hz animation frame.
pub const POLL_INTERVAL: Duration = Duration::from_millis(8);

const STICK_ENGAGE: f32 = 0.55;
const STICK_RELEASE: f32 = 0.35;
const INITIAL_REPEAT_DELAY: Duration = Duration::from_millis(350);
const REPEAT_INTERVAL: Duration = Duration::from_millis(90);

/// Owns the platform controller context and translates it into XMB actions.
///
/// Failure to access `/dev/input` is intentionally non-fatal: keyboard input
/// remains usable and the warning tells the user what functionality was lost.
pub struct ControllerInput {
    gilrs: Option<Gilrs>,
    navigation: Navigation,
}

impl ControllerInput {
    pub fn new(enabled: bool) -> Self {
        if !enabled {
            tracing::info!("game-controller input disabled");
            return Self {
                gilrs: None,
                navigation: Navigation::default(),
            };
        }

        match GilrsBuilder::new().with_force_feedback(false).build() {
            Ok(gilrs) => {
                let connected = gilrs.gamepads().count();
                tracing::info!(connected, "game-controller input ready");
                Self {
                    gilrs: Some(gilrs),
                    navigation: Navigation::default(),
                }
            }
            Err(err) => {
                tracing::warn!(%err, "game-controller input unavailable; keyboard input still works");
                Self {
                    gilrs: None,
                    navigation: Navigation::default(),
                }
            }
        }
    }

    /// Drain device events and return every action due at `now`.
    ///
    /// Events must still be drained while inactive so GilRs' cached button
    /// state and hot-plug list remain current.  Actions and held state are
    /// discarded in that case, which prevents the background shell reacting
    /// to controls intended for a running game.
    ///
    /// The guide button is the deliberate exception.  Controllers are read
    /// straight from `/dev/input` rather than through Wayland, so it reaches
    /// the shell even while a game holds the keyboard — which is the only
    /// reason a user can get back out of that game at all.
    pub fn poll(&mut self, now: Duration, active: bool) -> Vec<Action> {
        let Some(gilrs) = self.gilrs.as_mut() else {
            return Vec::new();
        };

        let mut actions = Vec::new();
        while let Some(event) = gilrs.next_event() {
            match event.event {
                EventType::Connected => {
                    let gamepad = gilrs.gamepad(event.id);
                    tracing::info!(
                        id = %event.id,
                        name = gamepad.name(),
                        mapping = ?gamepad.mapping_source(),
                        "controller connected"
                    );
                }
                EventType::Disconnected => {
                    tracing::info!(id = %event.id, "controller disconnected");
                }
                EventType::ButtonPressed(button, code) => {
                    let action = action_for_button(button, code.into_u32());
                    // Every press is logged: a button that does nothing is
                    // otherwise indistinguishable from a broken controller,
                    // and this names both what the mapping made of it and the
                    // raw code the kernel sent.
                    tracing::debug!(
                        ?button,
                        code = format_args!("{:#x}", code.into_u32()),
                        ?action,
                        "controller button"
                    );
                    if let Some(action) = action {
                        actions.push(action);
                    }
                }
                _ => {}
            }
        }

        if !active {
            self.navigation.reset();
            actions.retain(|action| *action == Action::Guide);
            return actions;
        }

        let mut pressed = [false; Direction::COUNT];
        let mut x = 0.0_f32;
        let mut y = 0.0_f32;

        // The shoulder bumpers deliberately do not appear here: they move
        // between displays, which is a press rather than something that repeats
        // while held.
        for (_, gamepad) in gilrs.gamepads() {
            pressed[Direction::Left.index()] |= gamepad.is_pressed(Button::DPadLeft);
            pressed[Direction::Right.index()] |= gamepad.is_pressed(Button::DPadRight);
            pressed[Direction::Up.index()] |= gamepad.is_pressed(Button::DPadUp);
            pressed[Direction::Down.index()] |= gamepad.is_pressed(Button::DPadDown);

            // Some older mappings expose a D-pad only as axes.  Prefer the
            // largest absolute value from any connected controller.
            x = larger_axis(x, gamepad.value(Axis::LeftStickX));
            x = larger_axis(x, gamepad.value(Axis::DPadX));
            y = larger_axis(y, gamepad.value(Axis::LeftStickY));
            y = larger_axis(y, gamepad.value(Axis::DPadY));
        }

        self.navigation.update(now, pressed, x, y, &mut actions);
        actions
    }
}

/// Linux input codes, packed the way GilRs reports them: `(EV_KEY << 16) | code`.
///
/// These are the kernel's own names for the physical buttons, before any
/// mapping database has an opinion about them.
mod evdev {
    const EV_KEY: u32 = 0x01;
    const fn key(code: u32) -> u32 {
        (EV_KEY << 16) | code
    }

    /// `BTN_A` is the same code; the gamepad spelling is the modern one.
    pub const BTN_SOUTH: u32 = key(0x130);
    /// Likewise `BTN_B`.
    pub const BTN_EAST: u32 = key(0x131);
    pub const BTN_SELECT: u32 = key(0x13a);
    pub const BTN_START: u32 = key(0x13b);
    pub const BTN_MODE: u32 = key(0x13c);
    /// The shoulder bumpers, L1 and R1.
    pub const BTN_TL: u32 = key(0x136);
    pub const BTN_TR: u32 = key(0x137);

    /// Pads that present as a plain joystick rather than a gamepad number
    /// their buttons from `BTN_TRIGGER` instead, so the first two are where
    /// the primary and secondary actions live.
    pub const BTN_TRIGGER: u32 = key(0x120);
    pub const BTN_THUMB: u32 = key(0x121);
}

/// Translate a button press into an action.
///
/// The mapping database is consulted first, because it is what knows how a
/// particular pad's face buttons are physically arranged. When it has no
/// opinion — GilRs reports [`Button::Unknown`] for anything its SDL entry or
/// the kernel's own mapping does not cover — the raw Linux code is used
/// instead. Without that fallback a pad missing from the database has a dead
/// A button and no way to launch anything.
fn action_for_button(button: Button, code: u32) -> Option<Action> {
    match button {
        // Xbox A / PlayStation Cross / Nintendo B / Steam Deck A.
        Button::South => return Some(Action::Launch),
        // Xbox B / PlayStation Circle / Nintendo A / Steam Deck B.
        Button::East => return Some(Action::Back),
        // Xbox Guide / PlayStation button / Steam Deck STEAM.
        Button::Mode => return Some(Action::Guide),
        Button::Start => return Some(Action::Launch),
        Button::Select => return Some(Action::Guide),
        // The shoulder buttons move between displays. GilRs calls the bumpers
        // `LeftTrigger`; the analogue triggers behind them are `*Trigger2`.
        Button::LeftTrigger => return Some(Action::PrevScreen),
        Button::RightTrigger => return Some(Action::NextScreen),
        Button::Unknown => {}
        _ => return None,
    }

    match code {
        evdev::BTN_SOUTH | evdev::BTN_START | evdev::BTN_TRIGGER => Some(Action::Launch),
        evdev::BTN_EAST | evdev::BTN_THUMB => Some(Action::Back),
        evdev::BTN_MODE | evdev::BTN_SELECT => Some(Action::Guide),
        evdev::BTN_TL => Some(Action::PrevScreen),
        evdev::BTN_TR => Some(Action::NextScreen),
        _ => None,
    }
}

fn larger_axis(current: f32, candidate: f32) -> f32 {
    if candidate.abs() > current.abs() {
        candidate
    } else {
        current
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    Left,
    Right,
    Up,
    Down,
}

impl Direction {
    const ALL: [Self; 4] = [Self::Left, Self::Right, Self::Up, Self::Down];
    const COUNT: usize = Self::ALL.len();

    const fn index(self) -> usize {
        match self {
            Self::Left => 0,
            Self::Right => 1,
            Self::Up => 2,
            Self::Down => 3,
        }
    }

    const fn action(self) -> Action {
        match self {
            Self::Left => Action::Left,
            Self::Right => Action::Right,
            Self::Up => Action::Up,
            Self::Down => Action::Down,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum AxisDirection {
    Negative,
    Positive,
    #[default]
    Neutral,
}

impl AxisDirection {
    fn update(self, value: f32) -> Self {
        match self {
            Self::Negative if value <= -STICK_RELEASE => Self::Negative,
            Self::Positive if value >= STICK_RELEASE => Self::Positive,
            _ if value <= -STICK_ENGAGE => Self::Negative,
            _ if value >= STICK_ENGAGE => Self::Positive,
            _ => Self::Neutral,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct HeldDirection {
    active: bool,
    next_repeat: Option<Duration>,
}

#[derive(Debug, Default)]
struct Navigation {
    x: AxisDirection,
    y: AxisDirection,
    held: [HeldDirection; Direction::COUNT],
}

impl Navigation {
    fn reset(&mut self) {
        *self = Self::default();
    }

    fn update(
        &mut self,
        now: Duration,
        mut pressed: [bool; Direction::COUNT],
        x: f32,
        y: f32,
        actions: &mut Vec<Action>,
    ) {
        self.x = self.x.update(x);
        self.y = self.y.update(y);

        pressed[Direction::Left.index()] |= self.x == AxisDirection::Negative;
        pressed[Direction::Right.index()] |= self.x == AxisDirection::Positive;
        // GilRs normalises stick Y so positive is up on every platform.
        pressed[Direction::Up.index()] |= self.y == AxisDirection::Positive;
        pressed[Direction::Down.index()] |= self.y == AxisDirection::Negative;

        for direction in Direction::ALL {
            let active = pressed[direction.index()];
            let held = &mut self.held[direction.index()];

            if !active {
                *held = HeldDirection::default();
                continue;
            }

            if !held.active {
                held.active = true;
                held.next_repeat = Some(now + INITIAL_REPEAT_DELAY);
                actions.push(direction.action());
                continue;
            }

            if held.next_repeat.is_some_and(|deadline| now >= deadline) {
                // Schedule from `now`, rather than the old deadline, so a
                // stalled process never emits a burst of stale repeats.
                held.next_repeat = Some(now + REPEAT_INTERVAL);
                actions.push(direction.action());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn update(
        navigation: &mut Navigation,
        millis: u64,
        pressed: [bool; Direction::COUNT],
        x: f32,
        y: f32,
    ) -> Vec<Action> {
        let mut actions = Vec::new();
        navigation.update(Duration::from_millis(millis), pressed, x, y, &mut actions);
        actions
    }

    #[test]
    fn digital_navigation_fires_once_then_repeats() {
        let mut navigation = Navigation::default();
        let mut pressed = [false; Direction::COUNT];
        pressed[Direction::Down.index()] = true;

        assert_eq!(
            update(&mut navigation, 0, pressed, 0.0, 0.0),
            [Action::Down]
        );
        assert!(update(&mut navigation, 349, pressed, 0.0, 0.0).is_empty());
        assert_eq!(
            update(&mut navigation, 350, pressed, 0.0, 0.0),
            [Action::Down]
        );
        assert!(update(&mut navigation, 439, pressed, 0.0, 0.0).is_empty());
        assert_eq!(
            update(&mut navigation, 440, pressed, 0.0, 0.0),
            [Action::Down]
        );

        pressed[Direction::Down.index()] = false;
        assert!(update(&mut navigation, 450, pressed, 0.0, 0.0).is_empty());
        pressed[Direction::Down.index()] = true;
        assert_eq!(
            update(&mut navigation, 451, pressed, 0.0, 0.0),
            [Action::Down]
        );
    }

    #[test]
    fn stick_uses_deadzone_hysteresis() {
        let mut navigation = Navigation::default();
        let none = [false; Direction::COUNT];

        assert!(update(&mut navigation, 0, none, 0.54, 0.0).is_empty());
        assert_eq!(update(&mut navigation, 1, none, 0.56, 0.0), [Action::Right]);
        // Moving below the engage threshold does not release a held stick.
        assert!(update(&mut navigation, 20, none, 0.40, 0.0).is_empty());
        // It releases below the lower threshold and can engage again.
        assert!(update(&mut navigation, 21, none, 0.34, 0.0).is_empty());
        assert_eq!(
            update(&mut navigation, 22, none, 0.60, 0.0),
            [Action::Right]
        );
    }

    #[test]
    fn both_axes_can_navigate_on_the_same_poll() {
        let mut navigation = Navigation::default();
        let actions = update(&mut navigation, 0, [false; Direction::COUNT], -0.9, 0.9);
        assert_eq!(actions, [Action::Left, Action::Up]);
    }

    #[test]
    fn reset_forgets_held_controls() {
        let mut navigation = Navigation::default();
        let none = [false; Direction::COUNT];
        assert_eq!(update(&mut navigation, 0, none, 0.8, 0.0), [Action::Right]);
        navigation.reset();
        assert_eq!(update(&mut navigation, 1, none, 0.8, 0.0), [Action::Right]);
    }

    #[test]
    fn mapped_face_buttons_drive_the_bar() {
        // The raw code is ignored whenever the mapping had an opinion, because
        // it is the mapping that knows how this pad is physically laid out.
        assert_eq!(
            action_for_button(Button::South, 0),
            Some(Action::Launch),
            "A launches"
        );
        assert_eq!(action_for_button(Button::East, 0), Some(Action::Back));
        assert_eq!(action_for_button(Button::Mode, 0), Some(Action::Guide));
        assert_eq!(action_for_button(Button::North, 0), None);
    }

    #[test]
    fn unmapped_buttons_fall_back_to_their_kernel_code() {
        // A pad missing from the mapping database still has to work: this is
        // what stops A being a dead button on an unrecognised controller.
        assert_eq!(
            action_for_button(Button::Unknown, evdev::BTN_SOUTH),
            Some(Action::Launch)
        );
        assert_eq!(
            action_for_button(Button::Unknown, evdev::BTN_EAST),
            Some(Action::Back)
        );
        assert_eq!(
            action_for_button(Button::Unknown, evdev::BTN_MODE),
            Some(Action::Guide)
        );
        // Joystick-style pads number their buttons from BTN_TRIGGER.
        assert_eq!(
            action_for_button(Button::Unknown, evdev::BTN_TRIGGER),
            Some(Action::Launch)
        );
        assert_eq!(action_for_button(Button::Unknown, 0xdead), None);
    }

    #[test]
    fn evdev_codes_match_the_kernel_headers() {
        // (EV_KEY << 16) | code, as GilRs packs them.
        assert_eq!(evdev::BTN_SOUTH, 0x1_0130);
        assert_eq!(evdev::BTN_EAST, 0x1_0131);
        assert_eq!(evdev::BTN_START, 0x1_013b);
        assert_eq!(evdev::BTN_MODE, 0x1_013c);
    }

    #[test]
    fn chooses_the_axis_furthest_from_rest() {
        assert_eq!(larger_axis(0.4, -0.8), -0.8);
        assert_eq!(larger_axis(-0.8, 0.5), -0.8);
    }
}
