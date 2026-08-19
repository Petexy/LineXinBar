//! Game-controller input for the shell.
//!
//! Wayland deliberately has no gamepad protocol.  A shell therefore has to
//! read controllers separately from its Wayland keyboard.  GilRs gives us
//! hot-plugging and the same SDL-compatible button mapping Steam exposes via
//! `SDL_GAMECONTROLLERCONFIG`, including the Steam Deck's built-in controls.
//!
//! One pad escapes it entirely.  The second-generation Steam Controller has no
//! kernel gamepad driver, so it has no joystick node for GilRs to open. It used
//! to reach the shell as a lizard-mode keyboard — the firmware pretending to be
//! one — with [`crate::steam_hid`] supplying only the Steam button, which is the
//! single control that has no keystroke to send.
//!
//! Launching Steam ends that: Steam claims the pad, writes lizard mode off, and
//! every keystroke stops. So the whole pad is read from its HID report now, and
//! the compositor drops the lizard keyboard so the two cannot both arrive.
//!
//! Reading a pad this way means every application on the machine can read the
//! same pad, because a controller never passes through the compositor at all.
//! That is fine for every button but one: the guide button is the way *out* of
//! an application, and an application that could see it could take it. So the
//! pads GilRs reads here are pads [`crate::pad_guard`] has already taken apart
//! — the shell reads the guide button from the guard, and GilRs reads a
//! stand-in device with everything else on it.

use std::time::Duration;

use gilrs::{Axis, Button, EventType, Gilrs, GilrsBuilder, MappingSource};

use crate::model::Action;
use crate::pad_guard::PadGuard;
use crate::steam_hid::{Buttons, SteamPad};

/// Controller state is sampled often enough that input never feels tied to a
/// 30/60 Hz animation frame.
pub const POLL_INTERVAL: Duration = Duration::from_millis(8);

const STICK_ENGAGE: f32 = 0.55;
const STICK_RELEASE: f32 = 0.35;

/// How long a direction has to be held before it starts stepping on its own,
/// and how long between steps after that.
///
/// The D-pad's rhythm and the keyboard's alike. Wayland hands a client one
/// press and one release and leaves everything in between to it, so the shell
/// invents the middle for both controls — the pad in [`Navigation::update`],
/// the arrow keys in `Shell::repeat_held_key` — and it invents the same
/// middle, because a held arrow that walked the bar at a different pace from a
/// held D-pad would make the two controls two interfaces.
pub const INITIAL_REPEAT_DELAY: Duration = Duration::from_millis(350);
/// See [`INITIAL_REPEAT_DELAY`].
pub const REPEAT_INTERVAL: Duration = Duration::from_millis(90);

/// One poll's worth of controller input.
///
/// Actions and pointer movement are reported separately because they are
/// gated separately: an action belongs to the shell and is dropped while an
/// application is being driven, and the pointer is the opposite — it exists
/// only while an application is in front, and the shell is merely the thing
/// holding the controller.
#[derive(Debug, Default)]
pub struct Poll {
    pub actions: Vec<Action>,
    /// Where the right stick is: `(x, y)`, each in −1.0..=1.0, with y positive
    /// *up* as every gamepad API normalises it. Raw, dead zone and all — the
    /// shell decides what a position means, because what it means depends on
    /// whether the pointer is being driven at all.
    pub right_stick: (f32, f32),
    /// The left stick, on the same terms: what scrolls while the pointer is
    /// being driven.
    ///
    /// An analogue control for an analogue job. A page is scrolled *by an
    /// amount*, which is what a stick pushed further says and a button can
    /// only say by being held for longer.
    pub scroll_stick: (f32, f32),
    /// Pointer buttons that changed this poll, as `(Linux button code, down)`.
    /// Empty unless one of the buttons the pointer borrows moved.
    pub clicks: Vec<(u32, bool)>,
    /// Arrow keys that changed this poll, as `(Linux key code, down)`, from the
    /// D-pad.
    ///
    /// Edges, not a state: a direction is pressed when the thumb puts it down
    /// and released when it comes up, and everything in between is the
    /// client's own key repeat — which is the repeat rate the user set, on the
    /// control that is meant to have one.
    pub arrows: Vec<(u32, bool)>,
    /// Whether there is a hand on the pad at all: any button down, any
    /// direction held, any stick pushed.
    ///
    /// Not "did the shell get anything out of this poll", which is what the
    /// four fields above are between them. This is the wider question, and it
    /// is asked of every control on the pad whether or not the shell is the one
    /// listening: with an application in front the buttons are dropped here and
    /// read from `/dev/input` by the game instead, and a thumb on them is still
    /// a thumb that is not on a keyboard. That is the only thing this answers,
    /// and the whole of what it is for: which control the shell last saw in the
    /// user's hands, which [`crate::settings::controller_in_hand`] remembers
    /// from one session to the next.
    pub stirred: bool,
}

/// Whether a stick has been pushed rather than merely left alone.
///
/// The same threshold navigating the bar takes, and deliberately not a smaller
/// one. What this decides is whether the user's hands are on the controller,
/// and a worn stick resting a little off centre would otherwise say they were,
/// for as long as the pad stayed plugged in: a keyboard user would be offered a
/// keyboard again every time the shell looked.
fn stick_is_pushed((x, y): (f32, f32)) -> bool {
    x.abs() >= STICK_ENGAGE || y.abs() >= STICK_ENGAGE
}

/// Owns the platform controller context and translates it into XMB actions.
///
/// Failure to access `/dev/input` is intentionally non-fatal: keyboard input
/// remains usable and the warning tells the user what functionality was lost.
pub struct ControllerInput {
    gilrs: Option<Gilrs>,
    navigation: Navigation,
    /// Which D-pad directions were down last poll, so the arrows can be
    /// reported as the edges they are. Kept here rather than beside the
    /// navigation state because it outlives it: navigation is reset the moment
    /// an application takes the screen, which is exactly when the D-pad starts
    /// being the arrows.
    dpad_held: [bool; Direction::COUNT],
    /// The second-generation Steam Controller, which GilRs can never see
    /// because the kernel gives that pad no gamepad node at all. Read from its
    /// HID report instead — see [`crate::steam_hid`].
    pad: SteamPad,
    /// The guide button of every other pad, which by then has been taken out of
    /// what GilRs is reading — see [`crate::pad_guard`]. It arrives here and
    /// nowhere else on the machine.
    guard: PadGuard,
    /// Whether the guide button now held down has already been spent on a
    /// chord, and so must not open the overlay when it comes back up.
    ///
    /// One flag for every pad on the machine, because the guide button is one
    /// control however many controllers are plugged in — and because a chord
    /// is answered once, not once per device that could have spelled it.
    guide_chorded: bool,
}

impl ControllerInput {
    pub fn new(enabled: bool) -> Self {
        if !enabled {
            tracing::info!("game-controller input disabled");
            return Self {
                gilrs: None,
                navigation: Navigation::default(),
                dpad_held: [false; Direction::COUNT],
                guide_chorded: false,
                pad: SteamPad::new(false),
                guard: PadGuard::new(false),
            };
        }

        match GilrsBuilder::new().with_force_feedback(false).build() {
            Ok(gilrs) => {
                let connected = gilrs.gamepads().count();
                tracing::info!(connected, "game-controller input ready");
                Self {
                    gilrs: Some(gilrs),
                    navigation: Navigation::default(),
                    dpad_held: [false; Direction::COUNT],
                    guide_chorded: false,
                    pad: SteamPad::new(true),
                    guard: PadGuard::new(true),
                }
            }
            Err(err) => {
                tracing::warn!(%err, "game-controller input unavailable; keyboard input still works");
                Self {
                    gilrs: None,
                    navigation: Navigation::default(),
                    dpad_held: [false; Direction::COUNT],
                    guide_chorded: false,
                    // Still worth watching: this pad's Steam button never came
                    // through GilRs in the first place, so whatever stopped
                    // GilRs from starting has not cost us this.
                    pad: SteamPad::new(true),
                    // And still worth guarding, for the same reason twice
                    // over: whatever stopped GilRs from reading a pad has not
                    // stopped anything else on the machine from reading one.
                    guard: PadGuard::new(true),
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
    /// The guide button is the deliberate exception, along with the two chords
    /// spelled on it and on Select.  Controllers are read straight from
    /// `/dev/input` rather than through Wayland, so they reach the shell even
    /// while a game holds the keyboard — which is the only reason a user can
    /// get back out of that game at all, or photograph it.
    ///
    /// The right stick and the two stick presses are read on the same terms
    /// and for the same reason: the pointer they drive is wanted *inside* the
    /// application, which is exactly when nothing else here is listened to.
    pub fn poll(&mut self, now: Duration, active: bool) -> Poll {
        let mut actions = Vec::new();
        let mut clicks = Vec::new();
        // Whether anything on the pad has been touched, which is a wider
        // question than any of the above and is asked of the raw controls
        // rather than of what the shell made of them: a shoulder button bound
        // to nothing is still a hand on the controller.
        let mut stirred = false;
        // Whether the guide button has already been spent, carried out of the
        // field and back into it so both halves below can read and write it
        // while GilRs is borrowed.
        let mut chorded = self.guide_chorded;

        // The guide button of every pad GilRs can see, which GilRs cannot see:
        // it is held back from the stand-in device the guard leaves in the
        // pad's place, so that no application gets it either. See
        // [`crate::pad_guard`].
        //
        // Read in two halves around everything else, and that ordering is the
        // whole of it. A press only begins a hold, so it is answered first,
        // before a chord below can be spelled on it; the release is answered
        // last, once every button that arrived in the same eight milliseconds
        // has had its chance to claim the hold. Doing both here would open the
        // guide on a screenshot chord whose two halves landed in one poll.
        let guide = self.guard.take_edges();
        let guide_held = self.guard.guide_held();
        stirred |= guide.pressed || guide_held;
        if guide.pressed {
            chorded = false;
        }

        // The Steam Controller first, and outside everything GilRs does: that
        // pad has no gamepad node for GilRs to have opened, so it is reachable
        // whether or not GilRs started at all — and once Steam claims the pad
        // and writes lizard mode off, this is the *only* way any of it arrives.
        let pad = self.pad.poll(now);
        if let Some(frame) = &pad {
            stirred |= !frame.held.is_empty();
            actions.extend(pad_actions(frame, &mut chorded));
            for (button, code) in PAD_CLICKS {
                if frame.pressed.has(*button) {
                    clicks.push((*code, true));
                }
                if frame.released.has(*button) {
                    clicks.push((*code, false));
                }
            }
            if !frame.pressed.is_empty() {
                tracing::debug!(pressed = ?frame.pressed, "steam controller buttons");
            }
        }

        if let Some(gilrs) = self.gilrs.as_mut() {
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
                        let code = code.into_u32();
                        // Before anything is made of it: a button this shell
                        // has no use for is still a thumb on the pad.
                        stirred = true;
                        if let Some(click) = pointer_button(button, code) {
                            clicks.push((click, true));
                        }
                        if is_guide(button, code) {
                            // A fresh hold; see the same line on the pad above.
                            chorded = false;
                        }
                        // Which of GilRs' two naming conventions this pad's
                        // names came from. Without it the face buttons cannot
                        // be told apart at all; see [`Layout`].
                        let layout = Layout::of(gilrs, event.id);
                        let action = chord_action(button, code, layout, select_is_held(gilrs))
                            .or_else(|| {
                                // Either source will do for the modifier. A
                                // guarded pad spells it through the guard, an
                                // unguarded one — a pad the guard could not
                                // take, on a machine with no `/dev/uinput` —
                                // still spells it through GilRs.
                                photograph_chord(button, code, guide_held || guide_is_held(gilrs))
                            })
                            .or_else(|| action_for_button(button, code, layout));
                        if action == Some(Action::Screenshot) {
                            chorded = true;
                        }
                        // Every press is logged: a button that does nothing is
                        // otherwise indistinguishable from a broken controller,
                        // and this names both what the mapping made of it and
                        // the raw code the kernel sent.
                        tracing::debug!(
                            ?button,
                            code = format_args!("{code:#x}"),
                            ?layout,
                            ?action,
                            "controller button"
                        );
                        if let Some(action) = action {
                            actions.push(action);
                        }
                    }
                    EventType::ButtonReleased(button, code) => {
                        let code = code.into_u32();
                        // The buttons the pointer borrows, and the guide
                        // button. Every other release is nothing: the shell
                        // acts on presses, and a menu row activated on the way
                        // back up would fire twice.
                        if let Some(click) = pointer_button(button, code) {
                            clicks.push((click, false));
                        }
                        if is_guide(button, code) {
                            if !chorded {
                                actions.push(Action::Guide);
                            }
                            chorded = false;
                        }
                    }
                    _ => {}
                }
            }
        }

        // The guarded guide button's other half, last of everything: by here a
        // chord spelled in this same poll has already claimed the hold, and a
        // hold nothing claimed is a tap. See the drain at the top.
        if guide.released {
            if !chorded {
                actions.push(Action::Guide);
            }
            chorded = false;
        }

        // Every stick and the D-pad, sampled once. Some of it is read whether
        // or not the shell is being driven: the pointer these move is wanted
        // *inside* the application, which is exactly when nothing else here is
        // listened to, so reading them above the gate below is what lets them
        // keep working once an application has taken the screen.
        let mut sticks = Sticks::read(self.gilrs.as_ref());
        if let Some(frame) = &pad {
            sticks.merge_pad(frame);
        }
        let right_stick = sticks.right;
        let scroll_stick = sticks.left;
        // Held rather than pressed, for both the D-pad and the sticks: this is
        // asked of every poll and a thumb resting on a direction is a hand on
        // the pad on all of them, not only the one it landed on.
        stirred |= sticks.dpad.iter().any(|held| *held)
            || stick_is_pushed(sticks.left)
            || stick_is_pushed(sticks.right);
        let arrows = self.arrow_edges(sticks.dpad);
        self.guide_chorded = chorded;

        if !active {
            self.navigation.reset();
            actions.retain(survives_an_application);
            return Poll {
                actions,
                right_stick,
                scroll_stick,
                clicks,
                arrows,
                // Whatever the gate above dropped, the hand that sent it was
                // still on the controller. This is the case that matters most:
                // with a game in front the shell listens to almost nothing, and
                // it is over a game that it has something in the corner of the
                // screen offering a pad a keyboard.
                stirred,
            };
        }

        self.navigation
            .update(now, sticks.dpad, sticks.left.0, sticks.left.1, &mut actions);
        Poll {
            actions,
            right_stick,
            scroll_stick,
            clicks,
            arrows,
            stirred,
        }
    }

    /// Which arrow keys went down or came up since the last poll.
    ///
    /// Reported whether or not the shell is being driven, like the sticks and
    /// for the same reason: the D-pad is the arrows precisely while an
    /// application is in front and the shell is listening to nothing else. The
    /// caller decides whether to send them, and the edges are tracked here so
    /// that a direction held across the moment it started counting still
    /// eventually reports its release.
    fn arrow_edges(&mut self, pressed: [bool; Direction::COUNT]) -> Vec<(u32, bool)> {
        let mut edges = Vec::new();
        for direction in Direction::ALL {
            let index = direction.index();
            if pressed[index] != self.dpad_held[index] {
                edges.push((direction.arrow_key(), pressed[index]));
            }
        }
        self.dpad_held = pressed;
        edges
    }
}

/// Where every stick and the D-pad are, read once per poll.
///
/// Each axis takes the furthest-from-rest answer any connected pad gives, so a
/// session with two pads plugged in is drivable from either without one of
/// them having to be chosen.
struct Sticks {
    left: (f32, f32),
    right: (f32, f32),
    /// The D-pad as four booleans, in [`Direction`] order.
    dpad: [bool; Direction::COUNT],
}

impl Sticks {
    /// Fold the Steam Controller's own reading in, on the same terms as a
    /// second GilRs pad: whichever is further from rest wins, and any D-pad
    /// held anywhere counts.
    fn merge_pad(&mut self, frame: &crate::steam_hid::Frame) {
        self.dpad[Direction::Left.index()] |= frame.held.has(Buttons::LEFT);
        self.dpad[Direction::Right.index()] |= frame.held.has(Buttons::RIGHT);
        self.dpad[Direction::Up.index()] |= frame.held.has(Buttons::UP);
        self.dpad[Direction::Down.index()] |= frame.held.has(Buttons::DOWN);

        self.left.0 = larger_axis(self.left.0, frame.left_stick.0);
        self.left.1 = larger_axis(self.left.1, frame.left_stick.1);
        self.right.0 = larger_axis(self.right.0, frame.right_stick.0);
        self.right.1 = larger_axis(self.right.1, frame.right_stick.1);
    }

    fn read(gilrs: Option<&Gilrs>) -> Self {
        let mut dpad = [false; Direction::COUNT];
        let (mut lx, mut ly) = (0.0_f32, 0.0_f32);
        let (mut rx, mut ry) = (0.0_f32, 0.0_f32);

        let Some(gilrs) = gilrs else {
            return Self {
                left: (lx, ly),
                right: (rx, ry),
                dpad,
            };
        };

        // The shoulder bumpers deliberately do not appear here: they move
        // between displays, which is a press rather than something that
        // repeats while held.
        for (_, gamepad) in gilrs.gamepads() {
            dpad[Direction::Left.index()] |= gamepad.is_pressed(Button::DPadLeft);
            dpad[Direction::Right.index()] |= gamepad.is_pressed(Button::DPadRight);
            dpad[Direction::Up.index()] |= gamepad.is_pressed(Button::DPadUp);
            dpad[Direction::Down.index()] |= gamepad.is_pressed(Button::DPadDown);

            // Some older mappings expose a D-pad only as axes. Folded in as
            // the four buttons it is rather than added to the left stick,
            // which is what it used to be: the two are one control for
            // navigating a menu, but a D-pad is the arrow keys and the stick
            // is the wheel, and those are not the same thing at all.
            let hat = hat_directions(gamepad.value(Axis::DPadX), gamepad.value(Axis::DPadY));
            for (held, from_hat) in dpad.iter_mut().zip(hat) {
                *held |= from_hat;
            }

            lx = larger_axis(lx, gamepad.value(Axis::LeftStickX));
            ly = larger_axis(ly, gamepad.value(Axis::LeftStickY));

            rx = larger_axis(rx, gamepad.value(Axis::RightStickX));
            ry = larger_axis(ry, gamepad.value(Axis::RightStickY));
        }

        Self {
            left: (lx, ly),
            right: (rx, ry),
            dpad,
        }
    }
}

/// How far a D-pad reported as an axis has to be pushed to count as pressed.
/// A hat switch only ever reports −1, 0 or 1, so anything short of the middle
/// will do; this is where a stick mis-detected as one would stop being noise.
const HAT_THRESHOLD: f32 = 0.5;

/// A D-pad reported as a pair of axes, as the four buttons it is, in
/// [`Direction`] order. Positive y is up, as everywhere else here.
fn hat_directions(x: f32, y: f32) -> [bool; Direction::COUNT] {
    let mut pressed = [false; Direction::COUNT];
    pressed[Direction::Left.index()] = x <= -HAT_THRESHOLD;
    pressed[Direction::Right.index()] = x >= HAT_THRESHOLD;
    pressed[Direction::Up.index()] = y >= HAT_THRESHOLD;
    pressed[Direction::Down.index()] = y <= -HAT_THRESHOLD;
    pressed
}

/// What each Steam Controller button means to the shell.
///
/// The same meanings the mapped pads get in [`action_for_button`], written out
/// directly because this pad needs no mapping database: its report was read off
/// the hardware, so which button is which is known exactly rather than guessed
/// from an SDL entry that does not cover it.
///
/// Absent on purpose: `X`, which belongs to whatever is running until `View` is
/// held with it; `View` itself, which is only that chord's modifier; `Steam`,
/// which acts on its release instead so the screenshot chord can claim the hold
/// (see [`is_guide`]); the two stick presses, which are the pointer's
/// and never the menu's; and the whole D-pad, which goes through [`Navigation`]
/// instead so that holding a direction repeats at the same rate every other
/// pad's does. Listed here as well it would walk the menu two rows per press.
const PAD_ACTIONS: &[(Buttons, Action)] = &[
    (Buttons::A, Action::Launch),
    (Buttons::B, Action::Back),
    (Buttons::Y, Action::Menu),
    // The pad's own Start, and the same button as every other pad's: Accept,
    // except over the on-screen keyboard. See [`Action::Submit`].
    (Buttons::MENU, Action::Submit),
    (Buttons::L1, Action::PrevScreen),
    (Buttons::R1, Action::NextScreen),
];

/// What one frame of the Steam Controller is worth to the shell.
///
/// `chorded` is the guide button's hold, carried between polls: whether the
/// button now down has already been spent on a chord and so must not open the
/// overlay when it comes back up.
///
/// Separate from [`ControllerInput::poll`] because it is the one part of
/// reading this pad that can be exercised without the pad: everything else
/// there is a device to open and a report to be handed. Two chords and an
/// edge-triggered guide button between them have more to get wrong than the
/// table lookup they surround.
fn pad_actions(frame: &crate::steam_hid::Frame, chorded: &mut bool) -> Vec<Action> {
    let mut actions = Vec::new();
    // A guide button going down begins a fresh hold with nothing spent on it.
    // Defensive rather than necessary: a release that never arrived must not
    // leave the one way out of a game dead for the rest of the session.
    if frame.pressed.has(Buttons::STEAM) {
        *chorded = false;
    }
    // The screenshot chord, spelled on this pad as Steam with the right bumper
    // — the same two controls as everywhere else.
    let photograph = frame.held.has(Buttons::STEAM) && frame.pressed.has(Buttons::R1);
    for (button, action) in PAD_ACTIONS {
        if !frame.pressed.has(*button) {
            continue;
        }
        // R1 moves to the next display on its own. Held with Steam it is the
        // other half of a chord and nothing else, or the picture would be taken
        // of one display and the flash answering for it drawn on the next.
        if photograph && *button == Buttons::R1 {
            continue;
        }
        actions.push(*action);
    }
    if photograph {
        actions.push(Action::Screenshot);
        *chorded = true;
    }
    // The guide button acts on the way back up rather than the way down, so the
    // chord above can claim it; `STEAM` is absent from `PAD_ACTIONS` for that
    // reason. See [`is_guide`].
    if frame.released.has(Buttons::STEAM) {
        if !*chorded {
            actions.push(Action::Guide);
        }
        *chorded = false;
    }
    // The keyboard chord, spelled on this pad as View with the left-hand face
    // button — the same two controls as everywhere else. `X` is deliberately
    // absent from `PAD_ACTIONS`, so it is this or nothing.
    if frame.held.has(Buttons::VIEW) && frame.pressed.has(Buttons::X) {
        actions.push(Action::Keyboard);
    }
    actions
}

/// Which mouse button each of the pad's borrowed buttons is.
///
/// The same two pairs as [`pointer_button`]: the face buttons where a hand
/// reaches without being taught, and the stick presses where a thumb already is
/// when it is aiming.
const PAD_CLICKS: &[(Buttons, u32)] = &[
    (Buttons::A, BTN_LEFT),
    (Buttons::R3, BTN_LEFT),
    (Buttons::B, BTN_RIGHT),
    (Buttons::L3, BTN_RIGHT),
];

/// Which mouse button a press is, if it is one.
///
/// Four buttons, in two pairs. `A` and `B` are where a hand reaches for
/// "click" without being taught, and the stick presses are where a thumb
/// already is when it is aiming — R3 under the stick doing the pointing, L3
/// beside it. Either pair does the same two things, so nothing has to be
/// remembered about which one this pad wants.
///
/// The caller only acts on these while the pointer is actually being driven,
/// which is the whole reason `A` can be taken at all: the shell drops every
/// action but the guide button and the keyboard chord once an application is
/// in front, so `A` is otherwise doing nothing there, and the user chose to
/// put this application in that state from the menu. Back in the shell's own
/// screens `A` is Launch again, because there the pointer is switched off.
///
/// The shoulders and triggers are never taken: they still move between
/// displays, and a game's are its own.
fn pointer_button(button: Button, code: u32) -> Option<u32> {
    match button {
        // Xbox A / PlayStation Cross / Nintendo B, and the stick under the
        // thumb that is aiming.
        Button::South | Button::RightThumb => return Some(BTN_LEFT),
        // Xbox B / PlayStation Circle / Nintendo A, and the other stick.
        Button::East | Button::LeftThumb => return Some(BTN_RIGHT),
        // A pad the mapping database has never heard of still has all four,
        // and the kernel's codes for them are not ambiguous the way the
        // left-hand face button's are.
        Button::Unknown => {}
        _ => return None,
    }
    match code {
        evdev::BTN_SOUTH | evdev::BTN_THUMBR => Some(BTN_LEFT),
        evdev::BTN_EAST | evdev::BTN_THUMBL => Some(BTN_RIGHT),
        _ => None,
    }
}

/// Linux button codes for the mouse, which is what `wl_pointer` — and so
/// `lxb_shell_v1.pointer_button` — carries.
pub const BTN_LEFT: u32 = 0x110;
pub const BTN_RIGHT: u32 = 0x111;

/// Whether Select is down on any connected pad.
///
/// Read from the devices rather than tracked, so a Select that went down
/// before this shell started still counts as held. The raw code is consulted
/// as well as the mapping, for the same reason presses are: a pad missing from
/// the database still has a Select button, and without this the chord's own
/// raw fallback could never fire, because the gate in front of it would never
/// open.
fn select_is_held(gilrs: &Gilrs) -> bool {
    gilrs.gamepads().any(|(_, gamepad)| {
        gamepad.is_pressed(Button::Select)
            || gamepad
                .state()
                .buttons()
                .any(|(code, data)| data.is_pressed() && code.into_u32() == evdev::BTN_SELECT)
    })
}

/// Whether a press, with Select held, is the keyboard chord.
///
/// Select and the left-hand face button together: X on an Xbox pad, Square on
/// a PlayStation one, Y on a Nintendo one. A chord rather than a button of its
/// own because there is no spare button on a controller — every face button
/// already means something to whatever is running.
///
/// Select does nothing by itself. It is a modifier, and a modifier that also
/// did something would make the chord two things at once: pressing it would
/// open the menu, the menu would close any board that was up, and the face
/// button after it could then only ever be opening one afresh — so the chord
/// could show the keyboard but never hide it. The menu has three ways in of
/// its own (the guide button, `B` from the bar, and the compositor's binding),
/// and does not need a fourth that costs the keyboard its off switch.
fn chord_action(button: Button, code: u32, layout: Layout, select_held: bool) -> Option<Action> {
    if !select_held {
        return None;
    }
    is_left_face(button, code, layout).then_some(Action::Keyboard)
}

/// Whether an action still counts once an application owns the screen.
///
/// Three of them do, and everything else is meant for the game rather than for
/// the shell behind it: the way back out of the application, the way to type
/// into it, and the way to photograph it. All three are read straight from
/// `/dev/input` rather than through Wayland, which is the only reason any of
/// them arrives at all while the game holds the keyboard — and a picture is
/// nearly always wanted of the game rather than of the bar, so this is the one
/// case that matters most for the last of them.
fn survives_an_application(action: &Action) -> bool {
    matches!(
        action,
        Action::Guide | Action::Keyboard | Action::Screenshot
    )
}

/// Whether a press, with the guide button held, is the screenshot chord.
///
/// The guide button and the right bumper: `STEAM`+`R1` on a Steam Controller or
/// a Deck, the PlayStation button and `R1` on a DualSense, Guide and `RB` on an
/// Xbox pad. The same two controls under every thumb, and the same chord the
/// Deck itself photographs a game with, which is the one a person arriving here
/// from that machine will try first.
///
/// A chord rather than a button of its own for the reason the keyboard's is
/// one: every face button and both shoulders already belong to whatever is
/// running, and there is no spare control on a controller. The bumper alone
/// still moves to the next display.
fn photograph_chord(button: Button, code: u32, guide_held: bool) -> Option<Action> {
    if !guide_held {
        return None;
    }
    is_right_bumper(button, code).then_some(Action::Screenshot)
}

/// Whether a press is the guide button — the one with a logo on it, in the
/// middle of the pad.
///
/// Named the same two ways every other button here is: by the mapping when
/// there is one, and by the kernel's own code when GilRs could name nothing.
/// Unlike the face buttons the two never disagree, because there is only one
/// button in the middle to be ambiguous about.
///
/// ## Why this button is answered on its release
///
/// It is the modifier of [`photograph_chord`], and a modifier that also did
/// something on the way down could not be one: the overlay would already be up
/// by the time the bumper arrived, and the picture would be of the overlay
/// rather than of whatever the user wanted a picture of. So the press only
/// begins a hold, and letting go is what opens or closes the guide — unless the
/// hold was spent on a chord, in which case nothing happens at all and the
/// button has done the one job it was pressed for.
///
/// The cost is a few milliseconds on the one control that has to work while a
/// game holds everything else, and a tap is still a tap. What is not paid for
/// is the alternative: opening the overlay on the press and closing it again
/// when the chord lands would leave the shell racing its own capture for the
/// frame the compositor photographs.
///
/// Select, the keyboard chord's modifier, needs none of this. It is not an
/// action in the first place — see [`chord_action`].
fn is_guide(button: Button, code: u32) -> bool {
    match button {
        Button::Mode => true,
        Button::Unknown => code == evdev::BTN_MODE,
        _ => false,
    }
}

/// Whether a press is the right bumper — the chord's other half, and on its own
/// the step to the next display.
fn is_right_bumper(button: Button, code: u32) -> bool {
    match button {
        // GilRs calls the bumpers `*Trigger`; the analogue triggers behind them
        // are `*Trigger2`.
        Button::RightTrigger => true,
        Button::Unknown => code == evdev::BTN_TR,
        _ => false,
    }
}

/// Whether the guide button is down on any connected pad.
///
/// Read from the devices rather than tracked, like [`select_is_held`] and for
/// the same reason: a button that went down before this shell started still
/// counts as held. The raw code is consulted as well as the mapping so that a
/// pad missing from the database can still spell the chord.
fn guide_is_held(gilrs: &Gilrs) -> bool {
    gilrs.gamepads().any(|(_, gamepad)| {
        gamepad.is_pressed(Button::Mode)
            || gamepad
                .state()
                .buttons()
                .any(|(code, data)| data.is_pressed() && code.into_u32() == evdev::BTN_MODE)
    })
}

/// Where a pad's button *names* came from, which is the one thing that decides
/// whether they can be believed.
///
/// GilRs uses two conventions, and they disagree about which of two adjacent
/// codes is the left-hand face button and which is the top one.
///
/// * A pad the SDL database knows is mapped by *index*. SDL's `x` is the third
///   button declared and its `y` is the fourth, which on an Xbox pad are
///   `0x133` and `0x134` — `BTN_X` and `BTN_Y` under the kernel's legacy
///   gamepad names. GilRs reports those as [`Button::West`] and
///   [`Button::North`], and the mapping is right, because SDL was written
///   against the physical layout.
/// * A pad it does not know falls back to the kernel's *positional* aliases
///   for the same codes, where `0x133` is `BTN_NORTH` and `0x134` is
///   `BTN_WEST`. The names then say nothing the raw code did not: a driver
///   modelled on xpad puts the left-hand button on `0x133`, and
///   `hid-playstation` really does emit `0x134` for Square, so the same
///   [`Button::North`] is the left-hand button on one pad and the top one on
///   the next.
///
/// GilRs will say which it did — that is [`gilrs::Gamepad::mapping_source`] —
/// and asking it is what lets the mapped case be read by name and the unmapped
/// case be read as the coin toss it is. Reading the name alone works on every
/// pad in the database and silently picks the wrong button on every pad that is
/// not; reading the code alone does the reverse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layout {
    /// An SDL mapping was found. `West` is the left-hand face button and
    /// `North` is the top one, whatever codes this driver sends for them.
    Mapped,
    /// No mapping. Both middle face buttons are guesses.
    Guessed,
}

impl Layout {
    fn of(gilrs: &Gilrs, id: gilrs::GamepadId) -> Self {
        match gilrs.gamepad(id).mapping_source() {
            MappingSource::SdlMappings => Self::Mapped,
            // `Driver` is GilRs' own positional table, and `None` is a pad it
            // could name nothing on at all. Neither knows the layout.
            MappingSource::Driver | MappingSource::None => Self::Guessed,
        }
    }
}

/// Whether a press is the chord's other half: the left-hand face button, the
/// one with `X` printed on it.
fn is_left_face(button: Button, code: u32, layout: Layout) -> bool {
    match layout {
        Layout::Mapped => matches!(button, Button::West),
        Layout::Guessed => is_middle_face(button, code),
    }
}

/// Whether a press is the *top* face button — Y on an Xbox pad, Triangle on a
/// PlayStation one — which raises the context menu, the way Triangle has opened
/// the options menu on a cross media bar since the first one.
fn is_top_face(button: Button, code: u32, layout: Layout) -> bool {
    match layout {
        Layout::Mapped => matches!(button, Button::North),
        Layout::Guessed => is_middle_face(button, code),
    }
}

/// Whether a press is *one of* the two middle face buttons on a pad nothing
/// knows the layout of — the left-hand one or the top one, with no way to say
/// which.
///
/// Both are therefore taken to be both, and Select is what separates the two
/// jobs: held, the press is the keyboard chord; alone, it raises the context
/// menu. This is what a pad missing from the SDL database used to be refused,
/// and refusing it was the wrong way round. Nothing else on the shell's own
/// screens wants either button — the left-hand one belongs to whatever is
/// running, and once an application is in front every action but the guide and
/// the chord is dropped anyway — so the whole cost of guessing is that two
/// buttons raise the context menu instead of one. The cost of not guessing was
/// a controller with no context menu at all.
fn is_middle_face(button: Button, code: u32) -> bool {
    match button {
        // GilRs' positional table: `0x133` is North to it and `0x134` is West,
        // and which physical button each is depends on the driver.
        Button::North | Button::West => matches!(code, evdev::BTN_X | evdev::BTN_WEST),
        // A pad presenting as a plain joystick has no gamepad codes at all, so
        // GilRs can name nothing on it. Its buttons run from `BTN_TRIGGER`, and
        // the third and fourth are where the other two face buttons sit — the
        // same ordering [`action_for_button`] already reads the first two under.
        Button::Unknown => matches!(
            code,
            evdev::BTN_X | evdev::BTN_WEST | evdev::BTN_THUMB2 | evdev::BTN_TOP
        ),
        _ => false,
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
    /// The left-hand face button under the kernel's *legacy* gamepad names —
    /// `BTN_X`, which is what xpad and every driver modelled on it emit for
    /// the button with `X` printed on it. The positional aliases in
    /// `input-event-codes.h` call this same code `BTN_NORTH`.
    pub const BTN_X: u32 = key(0x133);
    /// And under the positional aliases — `BTN_WEST`, which is what
    /// `hid-playstation` emits for Square, and what GilRs assumes when it has
    /// no mapping for a pad. The same code is `BTN_Y` to the legacy names.
    ///
    /// Neither is dependable on its own; see [`is_left_face`].
    pub const BTN_WEST: u32 = key(0x134);
    pub const BTN_SELECT: u32 = key(0x13a);
    pub const BTN_START: u32 = key(0x13b);
    pub const BTN_MODE: u32 = key(0x13c);
    /// The shoulder bumpers, L1 and R1.
    pub const BTN_TL: u32 = key(0x136);
    pub const BTN_TR: u32 = key(0x137);
    /// The stick presses, L3 and R3 — the pointer's two mouse buttons while
    /// the stick above them is driving it.
    pub const BTN_THUMBL: u32 = key(0x13d);
    pub const BTN_THUMBR: u32 = key(0x13e);

    /// Pads that present as a plain joystick rather than a gamepad number
    /// their buttons from `BTN_TRIGGER` instead, so the first two are where
    /// the primary and secondary actions live.
    pub const BTN_TRIGGER: u32 = key(0x120);
    pub const BTN_THUMB: u32 = key(0x121);
    /// And the third and fourth, which is where the other two face buttons are
    /// on a pad numbered this way. Read only as the ambiguous pair — see
    /// [`super::is_middle_face`] — because the ordering is a convention rather
    /// than a promise, and that pair is the one place a wrong guess costs
    /// nothing.
    pub const BTN_THUMB2: u32 = key(0x122);
    pub const BTN_TOP: u32 = key(0x123);
}

/// Translate a button press into an action.
///
/// The mapping database is consulted first, because it is what knows how a
/// particular pad's face buttons are physically arranged. When it has no
/// opinion — GilRs reports [`Button::Unknown`] for anything its SDL entry or
/// the kernel's own mapping does not cover — the raw Linux code is used
/// instead. Without that fallback a pad missing from the database has a dead
/// A button and no way to launch anything.
fn action_for_button(button: Button, code: u32, layout: Layout) -> Option<Action> {
    if is_top_face(button, code, layout) {
        return Some(Action::Menu);
    }
    match button {
        // Xbox A / PlayStation Cross / Nintendo B / Steam Deck A.
        Button::South => return Some(Action::Launch),
        // Xbox B / PlayStation Circle / Nintendo A / Steam Deck B.
        Button::East => return Some(Action::Back),
        // Start — Xbox Menu, the PlayStation Options button, Steam Deck's own
        // `≡`. Accept, like `A`, everywhere but over the on-screen keyboard,
        // where it is Enter and the way out of the board in one press. See
        // [`Action::Submit`].
        Button::Start => return Some(Action::Submit),
        // Select is missing on purpose: it is the keyboard chord's modifier
        // and nothing else. See [`chord_action`].
        //
        // So is the guide button — Xbox Guide, the PlayStation button, Steam
        // Deck STEAM — which is answered when it comes back up rather than
        // when it goes down, because on the way down it is the screenshot
        // chord's modifier. See [`is_guide`].
        // The shoulder buttons move between displays. GilRs calls the bumpers
        // `LeftTrigger`; the analogue triggers behind them are `*Trigger2`.
        Button::LeftTrigger => return Some(Action::PrevScreen),
        Button::RightTrigger => return Some(Action::NextScreen),
        Button::Unknown => {}
        _ => return None,
    }

    match code {
        evdev::BTN_SOUTH | evdev::BTN_TRIGGER => Some(Action::Launch),
        evdev::BTN_START => Some(Action::Submit),
        evdev::BTN_EAST | evdev::BTN_THUMB => Some(Action::Back),
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

    /// The arrow key this direction is, as a Linux key code.
    const fn arrow_key(self) -> u32 {
        match self {
            Self::Left => KEY_LEFT,
            Self::Right => KEY_RIGHT,
            Self::Up => KEY_UP,
            Self::Down => KEY_DOWN,
        }
    }
}

/// Linux key codes for the four arrows, which is what `wl_keyboard` — and so
/// `lxb_shell_v1.keyboard_key` — carries.
pub const KEY_UP: u32 = 103;
pub const KEY_LEFT: u32 = 105;
pub const KEY_RIGHT: u32 = 106;
pub const KEY_DOWN: u32 = 108;

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
            action_for_button(Button::South, 0, Layout::Mapped),
            Some(Action::Launch),
            "A launches"
        );
        assert_eq!(
            action_for_button(Button::East, 0, Layout::Mapped),
            Some(Action::Back)
        );
        // The guide button is not one of these: it is answered on its release,
        // because on the way down it is the screenshot chord's modifier.
        assert_eq!(action_for_button(Button::Mode, 0, Layout::Mapped), None);
        assert!(is_guide(Button::Mode, 0));
        // The top face button — Y, or Triangle — raises the context menu. Its
        // raw code is whatever the driver sends: `hid-playstation` reports
        // Triangle as `BTN_NORTH` and xpad reports Y as `BTN_WEST`, and the
        // mapping has already told us which physical button this is, so the
        // code is not consulted at all.
        for code in [evdev::BTN_WEST, evdev::BTN_X, 0] {
            assert_eq!(
                action_for_button(Button::North, code, Layout::Mapped),
                Some(Action::Menu),
                "the top face button on a mapped pad, code {code:#x}"
            );
        }
    }

    /// Start is its own action rather than a second `A`, under both namings
    /// and on the pad with no mapping at all.
    ///
    /// The shell folds it back into Accept everywhere the board is not up —
    /// see `Shell::on_action` — so what this is really asserting is that the
    /// one place they differ can still tell them apart. A Start that arrived
    /// as [`Action::Launch`] would press whichever letter the cursor happened
    /// to be standing on and leave the board up.
    #[test]
    fn start_is_not_the_same_button_as_accept() {
        assert_eq!(
            action_for_button(Button::Start, 0, Layout::Mapped),
            Some(Action::Submit)
        );
        assert_eq!(
            action_for_button(Button::Unknown, evdev::BTN_START, Layout::Guessed),
            Some(Action::Submit)
        );
        assert_eq!(pad_action(Buttons::MENU), Some(Action::Submit));
        // And the buttons it is not: `A` and a joystick-numbered pad's first
        // button are still Accept, which is what the fold makes Start look
        // like everywhere but the board.
        assert_eq!(
            action_for_button(Button::South, 0, Layout::Mapped),
            Some(Action::Launch)
        );
        assert_eq!(
            action_for_button(Button::Unknown, evdev::BTN_TRIGGER, Layout::Guessed),
            Some(Action::Launch)
        );
        // Start is nobody's pointer button either: it closes a board rather
        // than clicking anything.
        assert_eq!(pointer_button(Button::Start, evdev::BTN_START), None);
    }

    /// The bug this was written for: the context menu could not be raised from
    /// an 8BitDo — or from any other pad GilRs found no SDL mapping for.
    ///
    /// The two conventions GilRs names buttons under disagree about the pair of
    /// codes the middle face buttons use, and the old rule resolved that
    /// disagreement by giving the whole pair to the keyboard chord and leaving
    /// the context menu with no button at all. That is the wrong way round:
    /// Select already separates the two jobs, so both buttons can do both.
    #[test]
    fn an_unmapped_pads_middle_face_buttons_raise_the_context_menu() {
        // GilRs' positional table names `0x133` North and `0x134` West, and
        // which physical button each is depends on the driver — so both are
        // taken, and a bare press of either raises the menu.
        for (button, code) in [
            (Button::North, evdev::BTN_X),
            (Button::North, evdev::BTN_WEST),
            (Button::West, evdev::BTN_X),
            (Button::West, evdev::BTN_WEST),
        ] {
            assert_eq!(
                action_for_button(button, code, Layout::Guessed),
                Some(Action::Menu),
                "{button:?} {code:#x}"
            );
            // And the same press with Select held is still the keyboard, which
            // is the whole reason both buttons can be claimed.
            assert_eq!(
                chord_action(button, code, Layout::Guessed, true),
                Some(Action::Keyboard),
                "{button:?} {code:#x}"
            );
        }

        // A pad presenting as a plain joystick has no gamepad codes for GilRs
        // to name anything from, and its face buttons run from `BTN_TRIGGER`.
        for code in [evdev::BTN_THUMB2, evdev::BTN_TOP] {
            assert_eq!(
                action_for_button(Button::Unknown, code, Layout::Guessed),
                Some(Action::Menu),
                "{code:#x}"
            );
        }
        // But not the two below them, which are already A and B.
        for code in [evdev::BTN_TRIGGER, evdev::BTN_THUMB] {
            assert_ne!(
                action_for_button(Button::Unknown, code, Layout::Guessed),
                Some(Action::Menu),
                "{code:#x}"
            );
        }
    }

    /// A mapped pad is read by name alone, so the guesswork above can never
    /// reach it: `X` stays the chord's and only `Y` raises the menu.
    #[test]
    fn a_mapped_pads_left_face_button_is_never_the_context_menu() {
        assert!(is_left_face(Button::West, evdev::BTN_X, Layout::Mapped));
        assert!(!is_top_face(Button::West, evdev::BTN_X, Layout::Mapped));
        assert_eq!(
            action_for_button(Button::West, evdev::BTN_X, Layout::Mapped),
            None,
            "X alone belongs to whatever is running"
        );

        assert!(is_top_face(Button::North, evdev::BTN_WEST, Layout::Mapped));
        assert!(!is_left_face(
            Button::North,
            evdev::BTN_WEST,
            Layout::Mapped
        ));
        assert_eq!(
            chord_action(Button::North, evdev::BTN_WEST, Layout::Mapped, true),
            None,
            "Select + Y is not the keyboard"
        );
    }

    #[test]
    fn unmapped_buttons_fall_back_to_their_kernel_code() {
        // A pad missing from the mapping database still has to work: this is
        // what stops A being a dead button on an unrecognised controller.
        assert_eq!(
            action_for_button(Button::Unknown, evdev::BTN_SOUTH, Layout::Guessed),
            Some(Action::Launch)
        );
        assert_eq!(
            action_for_button(Button::Unknown, evdev::BTN_EAST, Layout::Guessed),
            Some(Action::Back)
        );
        // Including the guide button, which is recognised by its raw code the
        // same way — but on the way back up. See [`is_guide`].
        assert_eq!(
            action_for_button(Button::Unknown, evdev::BTN_MODE, Layout::Guessed),
            None
        );
        assert!(is_guide(Button::Unknown, evdev::BTN_MODE));
        // Joystick-style pads number their buttons from BTN_TRIGGER.
        assert_eq!(
            action_for_button(Button::Unknown, evdev::BTN_TRIGGER, Layout::Guessed),
            Some(Action::Launch)
        );
        assert_eq!(
            action_for_button(Button::Unknown, 0xdead, Layout::Guessed),
            None
        );
    }

    #[test]
    fn the_keyboard_needs_select_held_with_it() {
        // On its own the left-hand face button does nothing: it belongs to
        // whatever is running, and taking it would break every game that uses
        // it.
        assert_eq!(
            chord_action(Button::West, evdev::BTN_X, Layout::Mapped, false),
            None
        );
        assert_eq!(
            action_for_button(Button::West, evdev::BTN_X, Layout::Mapped),
            None
        );

        // Held with Select it is the keyboard.
        assert_eq!(
            chord_action(Button::West, evdev::BTN_X, Layout::Mapped, true),
            Some(Action::Keyboard),
            "Select + the left-hand face button is the keyboard chord"
        );

        // No other button becomes anything else while Select is held —
        // holding it and pressing A must still launch, not type.
        for button in [Button::South, Button::East, Button::Start] {
            assert_eq!(
                chord_action(button, 0, Layout::Mapped, true),
                None,
                "{button:?}"
            );
        }
        assert_eq!(
            chord_action(Button::Unknown, evdev::BTN_SOUTH, Layout::Guessed, true),
            None
        );
    }

    /// The earlier bug this file already carried a fix for: the chord worked on
    /// the pads the SDL database knows and quietly did nothing on the rest,
    /// because GilRs names the same physical button two different things
    /// depending on whether it has a mapping for the pad. Asking GilRs *which*
    /// naming it used is what replaced the guess, so the case still has to hold.
    #[test]
    fn the_chords_face_button_is_found_under_both_of_gilrs_names_for_it() {
        // Mapped: SDL's `x` is the third button declared, and GilRs calls it
        // West whatever code the driver sends.
        assert!(is_left_face(Button::West, evdev::BTN_X, Layout::Mapped));
        assert!(is_left_face(Button::West, evdev::BTN_WEST, Layout::Mapped));

        // Unmapped: the same physical button arrives as North, because GilRs is
        // reading the kernel's positional aliases, under which `0x133` is
        // `BTN_NORTH`. This is the case that did not work.
        assert!(is_left_face(Button::North, evdev::BTN_X, Layout::Guessed));

        // With no mapping and no name, both codes are taken: nothing is known
        // about the pad, and a chord that does nothing is the worse fault.
        assert!(is_left_face(Button::Unknown, evdev::BTN_X, Layout::Guessed));
        assert!(is_left_face(
            Button::Unknown,
            evdev::BTN_WEST,
            Layout::Guessed
        ));
        assert!(!is_left_face(
            Button::Unknown,
            evdev::BTN_SOUTH,
            Layout::Guessed
        ));

        // And nothing else is ever the left-hand face button, under either
        // naming.
        for button in [Button::South, Button::East, Button::Start, Button::Mode] {
            for layout in [Layout::Mapped, Layout::Guessed] {
                assert!(
                    !is_left_face(button, evdev::BTN_X, layout),
                    "{button:?} {layout:?}"
                );
            }
        }
    }

    /// The guide button with the right bumper photographs the display, and it
    /// is spelled the same way on a pad nothing knows the layout of: unlike
    /// the face buttons, neither of these two is ambiguous under either naming.
    #[test]
    fn the_guide_button_with_the_right_bumper_photographs_the_screen() {
        assert_eq!(
            photograph_chord(Button::RightTrigger, 0, true),
            Some(Action::Screenshot)
        );
        assert_eq!(
            photograph_chord(Button::Unknown, evdev::BTN_TR, true),
            Some(Action::Screenshot)
        );

        // The bumper on its own is still the step to the next display, which is
        // what the chord must not cost it.
        assert_eq!(photograph_chord(Button::RightTrigger, 0, false), None);
        assert_eq!(
            action_for_button(Button::RightTrigger, 0, Layout::Mapped),
            Some(Action::NextScreen)
        );

        // And nothing else spells it. The *left* bumper especially: it is the
        // step the other way, and a chord that took both would leave a pad
        // with the guide held unable to move between displays at all.
        for (button, code) in [
            (Button::LeftTrigger, evdev::BTN_TL),
            (Button::South, evdev::BTN_SOUTH),
            (Button::Mode, evdev::BTN_MODE),
            (Button::Unknown, evdev::BTN_TL),
        ] {
            assert_eq!(photograph_chord(button, code, true), None, "{button:?}");
        }
    }

    /// The guide button is the chord's modifier, so it cannot also act on the
    /// way down: the overlay would be up before the bumper arrived, and the
    /// picture would be of the overlay rather than of the game underneath it.
    #[test]
    fn the_guide_button_acts_on_its_release_and_not_its_press() {
        assert_eq!(action_for_button(Button::Mode, 0, Layout::Mapped), None);
        assert_eq!(
            action_for_button(Button::Unknown, evdev::BTN_MODE, Layout::Guessed),
            None
        );
        assert_eq!(pad_action(Buttons::STEAM), None);

        // What the release is recognised by, under both namings.
        assert!(is_guide(Button::Mode, 0));
        assert!(is_guide(Button::Unknown, evdev::BTN_MODE));

        // And nothing else is ever it — including the buttons whose raw codes
        // sit either side of `BTN_MODE`.
        for (button, code) in [
            (Button::Start, evdev::BTN_START),
            (Button::Select, evdev::BTN_SELECT),
            (Button::RightTrigger, evdev::BTN_TR),
            (Button::Unknown, evdev::BTN_START),
            (Button::Unknown, evdev::BTN_SELECT),
        ] {
            assert!(!is_guide(button, code), "{button:?} {code:#x}");
        }
    }

    /// Select is a modifier and nothing else. It used to open the menu as
    /// well, which was wrong twice over: the menu appeared behind every
    /// keyboard summoned with the chord, and — because the menu closes the
    /// board — the face button after it could only ever be opening one afresh,
    /// so the chord could show the keyboard but never hide it.
    #[test]
    fn select_does_nothing_on_its_own() {
        assert_eq!(
            action_for_button(Button::Select, evdev::BTN_SELECT, Layout::Mapped),
            None
        );
        // Including on a pad the mapping database has never heard of.
        assert_eq!(
            action_for_button(Button::Unknown, evdev::BTN_SELECT, Layout::Guessed),
            None
        );

        // The guide button still opens the menu — on its release now, rather
        // than its press — and so does `B` from the bar: Select was never the
        // only way in.
        assert!(is_guide(Button::Mode, 0));
        assert!(is_guide(Button::Unknown, evdev::BTN_MODE));
        assert_eq!(
            action_for_button(Button::East, 0, Layout::Mapped),
            Some(Action::Back)
        );
    }

    #[test]
    fn the_keyboard_chord_reaches_the_shell_past_a_running_application() {
        // The three actions an inactive shell still acts on. Everything else
        // is meant for the game that has the screen; these are the way out of
        // it, the way to type into it, and the way to photograph it.
        let mut actions = vec![
            Action::Guide,
            Action::Keyboard,
            Action::Screenshot,
            Action::Launch,
            Action::Left,
            Action::Back,
        ];
        actions.retain(survives_an_application);
        assert_eq!(
            actions,
            [Action::Guide, Action::Keyboard, Action::Screenshot]
        );
    }

    /// Two pairs of buttons click, and nothing else does: `A` and `B` where a
    /// hand reaches without being taught, and the stick presses where a thumb
    /// already is. Everything a game could plausibly have bound stays the
    /// game's.
    #[test]
    fn the_pointer_borrows_the_face_buttons_and_the_stick_presses() {
        assert_eq!(pointer_button(Button::South, 0), Some(BTN_LEFT));
        assert_eq!(pointer_button(Button::RightThumb, 0), Some(BTN_LEFT));
        assert_eq!(pointer_button(Button::East, 0), Some(BTN_RIGHT));
        assert_eq!(pointer_button(Button::LeftThumb, 0), Some(BTN_RIGHT));

        for button in [
            // The left-hand face button is the keyboard chord's other half.
            Button::West,
            Button::North,
            Button::Start,
            Button::Select,
            Button::Mode,
            // The shoulders still move between displays.
            Button::LeftTrigger,
            Button::RightTrigger,
        ] {
            assert_eq!(pointer_button(button, 0), None, "{button:?}");
        }

        // A pad missing from the mapping database still has all four.
        for (code, expected) in [
            (evdev::BTN_SOUTH, BTN_LEFT),
            (evdev::BTN_THUMBR, BTN_LEFT),
            (evdev::BTN_EAST, BTN_RIGHT),
            (evdev::BTN_THUMBL, BTN_RIGHT),
        ] {
            assert_eq!(
                pointer_button(Button::Unknown, code),
                Some(expected),
                "{code:#x}"
            );
        }
        assert_eq!(pointer_button(Button::Unknown, evdev::BTN_X), None);
    }

    /// `A` is a click and Launch at once, which is only safe because the two
    /// are never listened to at the same time: the shell drops every action
    /// but the guide button and the keyboard chord once an application is in
    /// front, and the pointer only exists while one is.
    #[test]
    fn a_face_button_is_a_click_only_where_it_is_not_a_menu_action() {
        assert_eq!(
            action_for_button(Button::South, 0, Layout::Mapped),
            Some(Action::Launch)
        );
        assert_eq!(pointer_button(Button::South, 0), Some(BTN_LEFT));

        let mut actions = vec![Action::Launch, Action::Back, Action::Guide];
        actions.retain(survives_an_application);
        assert_eq!(
            actions,
            [Action::Guide],
            "with an application in front, A and B reach nothing but the pointer"
        );

        // Neither stick press means anything to the bar in any state, so those
        // two can never be both a click and a navigation.
        assert_eq!(
            action_for_button(Button::RightThumb, 0, Layout::Mapped),
            None
        );
        assert_eq!(
            action_for_button(Button::LeftThumb, 0, Layout::Mapped),
            None
        );
        assert_eq!(
            chord_action(Button::RightThumb, 0, Layout::Mapped, true),
            None
        );
    }

    /// The D-pad is the arrow keys, and it is reported as the edges a key has
    /// rather than as a state — because what happens between them is the
    /// client's own repeat, at the rate the user set.
    #[test]
    fn the_dpad_reports_the_arrows_going_down_and_coming_up() {
        let mut input = ControllerInput::new(false);
        let mut pressed = [false; Direction::COUNT];
        assert!(input.arrow_edges(pressed).is_empty(), "nothing at rest");

        pressed[Direction::Down.index()] = true;
        assert_eq!(input.arrow_edges(pressed), vec![(KEY_DOWN, true)]);
        // Held is not pressed again: one press, then the client repeats it.
        assert!(input.arrow_edges(pressed).is_empty(), "held is not a press");

        // A second direction with the first still down is its own press.
        pressed[Direction::Right.index()] = true;
        assert_eq!(input.arrow_edges(pressed), vec![(KEY_RIGHT, true)]);

        // And letting go of everything releases both, and only once.
        let released = input.arrow_edges([false; Direction::COUNT]);
        assert_eq!(released.len(), 2, "{released:?}");
        assert!(released.contains(&(KEY_RIGHT, false)));
        assert!(released.contains(&(KEY_DOWN, false)));
        assert!(input.arrow_edges([false; Direction::COUNT]).is_empty());
    }

    /// The arrows are the kernel's codes, which is what the compositor feeds
    /// its keymap and so what the request carries.
    #[test]
    fn the_arrow_keys_are_the_kernel_codes() {
        assert_eq!(Direction::Up.arrow_key(), 103);
        assert_eq!(Direction::Left.arrow_key(), 105);
        assert_eq!(Direction::Right.arrow_key(), 106);
        assert_eq!(Direction::Down.arrow_key(), 108);
    }

    /// A pad whose D-pad is only an axis pair still has arrow keys — and its
    /// D-pad must not end up in the left stick, which is the wheel. It used to,
    /// back when the two were one control for walking down a menu.
    #[test]
    fn a_dpad_reported_as_axes_is_still_four_buttons() {
        let none = [false; Direction::COUNT];
        assert_eq!(hat_directions(0.0, 0.0), none);

        let mut left_and_up = none;
        left_and_up[Direction::Left.index()] = true;
        left_and_up[Direction::Up.index()] = true;
        assert_eq!(hat_directions(-1.0, 1.0), left_and_up, "y is positive up");

        let mut right_and_down = none;
        right_and_down[Direction::Right.index()] = true;
        right_and_down[Direction::Down.index()] = true;
        assert_eq!(hat_directions(1.0, -1.0), right_and_down);

        // A hat is −1, 0 or 1; anything in between is a stick that has been
        // mistaken for one, and half way is where it starts counting.
        assert_eq!(hat_directions(0.4, -0.4), none);
    }

    #[test]
    fn evdev_codes_match_the_kernel_headers() {
        // (EV_KEY << 16) | code, as GilRs packs them.
        assert_eq!(evdev::BTN_SOUTH, 0x1_0130);
        assert_eq!(evdev::BTN_EAST, 0x1_0131);
        // The two names for the two adjacent codes the face buttons use, and
        // the reason [`is_left_face`] has to read both.
        assert_eq!(evdev::BTN_X, 0x1_0133);
        assert_eq!(evdev::BTN_WEST, 0x1_0134);
        assert_eq!(evdev::BTN_SELECT, 0x1_013a);
        assert_eq!(evdev::BTN_START, 0x1_013b);
        assert_eq!(evdev::BTN_MODE, 0x1_013c);
        assert_eq!(evdev::BTN_THUMBL, 0x1_013d);
        assert_eq!(evdev::BTN_THUMBR, 0x1_013e);
        // The mouse buttons are the pointer protocol's own, not packed input
        // codes: they go out over the wire as wl_pointer numbers them.
        assert_eq!(BTN_LEFT, 0x110);
        assert_eq!(BTN_RIGHT, 0x111);
    }

    #[test]
    fn chooses_the_axis_furthest_from_rest() {
        assert_eq!(larger_axis(0.4, -0.8), -0.8);
        assert_eq!(larger_axis(-0.8, 0.5), -0.8);
    }

    /// A stick says there is a hand on the pad only once it has been pushed as
    /// far as navigating the bar takes.
    ///
    /// The threshold is the point of it. What this decides is whether the shell
    /// goes on offering a keyboard to a controller, and a worn stick resting a
    /// little off centre would answer yes for as long as the pad stayed plugged
    /// in — so a keyboard user would find the corner chip back every time they
    /// looked, with nobody having touched anything.
    #[test]
    fn only_a_stick_that_has_been_pushed_says_the_pad_is_in_hand() {
        assert!(!stick_is_pushed((0.0, 0.0)));
        assert!(!stick_is_pushed((0.2, -0.2)), "a stick that rests crooked");
        assert!(!stick_is_pushed((0.54, 0.0)));

        assert!(stick_is_pushed((0.56, 0.0)));
        assert!(stick_is_pushed((0.0, -0.9)), "and in every direction");
        assert!(stick_is_pushed((-0.7, 0.0)));
    }

    fn pad_action(button: Buttons) -> Option<Action> {
        PAD_ACTIONS
            .iter()
            .find(|(candidate, _)| *candidate == button)
            .map(|(_, action)| *action)
    }

    /// The Steam Controller drives the bar the same way every other pad does.
    #[test]
    fn the_steam_controller_maps_to_the_same_actions() {
        assert_eq!(pad_action(Buttons::A), Some(Action::Launch));
        assert_eq!(pad_action(Buttons::B), Some(Action::Back));
        // Steam is absent on purpose: like every other pad's guide button it
        // is answered when it comes back up, so that holding it can spell the
        // screenshot chord. See [`is_guide`].
        assert_eq!(pad_action(Buttons::STEAM), None);
        assert_eq!(pad_action(Buttons::MENU), Some(Action::Submit));
        assert_eq!(pad_action(Buttons::L1), Some(Action::PrevScreen));
        assert_eq!(pad_action(Buttons::R1), Some(Action::NextScreen));
    }

    /// The bug this table was written around: the D-pad already goes through
    /// [`Navigation`], which is what gives a held direction its initial delay
    /// and repeat rate. Listed as a plain action *as well*, every press would
    /// move two rows — once from the table and once from the navigation.
    #[test]
    fn the_dpad_is_not_also_a_plain_action() {
        for direction in [Buttons::UP, Buttons::DOWN, Buttons::LEFT, Buttons::RIGHT] {
            assert_eq!(pad_action(direction), None, "{direction:?} would double");
        }
        // It reaches navigation instead, which is where the repeat lives.
        let mut sticks = Sticks {
            left: (0.0, 0.0),
            right: (0.0, 0.0),
            dpad: [false; Direction::COUNT],
        };
        let frame = pad_frame(Buttons::DOWN, Buttons::DOWN, Buttons::empty());
        sticks.merge_pad(&frame);
        assert!(sticks.dpad[Direction::Down.index()]);
        assert!(!sticks.dpad[Direction::Up.index()]);
    }

    /// One poll of the Steam Controller, written as what is down and what
    /// moved since the last one.
    fn pad_frame(held: Buttons, pressed: Buttons, released: Buttons) -> crate::steam_hid::Frame {
        crate::steam_hid::Frame {
            held,
            pressed,
            released,
            ..Default::default()
        }
    }

    /// Steam held with R1 photographs the screen, and does only that: the
    /// bumper does not also step to the next display, or the picture would be
    /// taken of one screen and the flash answering for it drawn on the other.
    #[test]
    fn the_pads_screenshot_chord_takes_its_bumper_with_it() {
        let mut chorded = false;
        let steam_down = pad_frame(Buttons::STEAM, Buttons::STEAM, Buttons::empty());
        assert_eq!(
            pad_actions(&steam_down, &mut chorded),
            Vec::new(),
            "the guide button does nothing on the way down"
        );

        let with_bumper = pad_frame(
            Buttons::STEAM.union(Buttons::R1),
            Buttons::R1,
            Buttons::empty(),
        );
        assert_eq!(
            pad_actions(&with_bumper, &mut chorded),
            vec![Action::Screenshot]
        );
        assert!(chorded, "the hold has been spent");

        // And letting go of Steam afterwards opens nothing: the button did the
        // one job it was pressed for.
        let steam_up = pad_frame(Buttons::empty(), Buttons::empty(), Buttons::STEAM);
        assert_eq!(pad_actions(&steam_up, &mut chorded), Vec::new());
        assert!(!chorded, "and the next press begins a fresh hold");
    }

    /// A tap of the guide button on its own still opens the overlay, which is
    /// the whole reason a user can get back out of a game.
    #[test]
    fn the_pads_guide_button_opens_the_overlay_when_it_comes_back_up() {
        let mut chorded = false;
        let down = pad_frame(Buttons::STEAM, Buttons::STEAM, Buttons::empty());
        assert_eq!(pad_actions(&down, &mut chorded), Vec::new());

        let up = pad_frame(Buttons::empty(), Buttons::empty(), Buttons::STEAM);
        assert_eq!(pad_actions(&up, &mut chorded), vec![Action::Guide]);

        // Including a tap short enough that both edges land in one poll.
        let tapped = pad_frame(Buttons::empty(), Buttons::STEAM, Buttons::STEAM);
        assert_eq!(pad_actions(&tapped, &mut chorded), vec![Action::Guide]);
    }

    /// The bumper on its own is untouched by the chord: it is how a session
    /// with two displays moves between them, and it is pressed far more often
    /// than the chord is.
    #[test]
    fn the_pads_bumper_alone_still_moves_between_displays() {
        let mut chorded = false;
        let bumper = pad_frame(Buttons::R1, Buttons::R1, Buttons::empty());
        assert_eq!(pad_actions(&bumper, &mut chorded), vec![Action::NextScreen]);
        assert!(!chorded);
    }

    /// `X` belongs to whatever is running, and `View` is only the chord's
    /// modifier. Either one acting alone would take a button from a game.
    #[test]
    fn the_pads_chord_halves_do_nothing_apart() {
        assert_eq!(pad_action(Buttons::X), None);
        assert_eq!(pad_action(Buttons::VIEW), None);
        // Nor do the stick presses, which are the pointer's alone.
        assert_eq!(pad_action(Buttons::L3), None);
        assert_eq!(pad_action(Buttons::R3), None);
    }

    /// The pad's mouse buttons are the same two pairs the mapped pads lend the
    /// pointer, so a hand moving between controllers finds them in one place.
    #[test]
    fn the_pads_clicks_match_every_other_pads() {
        let click = |button: Buttons| {
            PAD_CLICKS
                .iter()
                .find(|(candidate, _)| *candidate == button)
                .map(|(_, code)| *code)
        };
        assert_eq!(click(Buttons::A), Some(BTN_LEFT));
        assert_eq!(click(Buttons::R3), Some(BTN_LEFT));
        assert_eq!(click(Buttons::B), Some(BTN_RIGHT));
        assert_eq!(click(Buttons::L3), Some(BTN_RIGHT));
        assert_eq!(click(Buttons::X), None);
        assert_eq!(click(Buttons::STEAM), None);
    }

    /// The whole point of reading this pad from hidraw: the way out of a
    /// running application has to survive the gate that drops everything else,
    /// and so does the chord that photographs it. Neither is in the table —
    /// one is answered on a release and the other is a chord — so what the
    /// table must not do is smuggle anything *else* past the gate.
    #[test]
    fn only_the_three_outside_actions_survive_a_running_application() {
        let mut actions: Vec<Action> = PAD_ACTIONS.iter().map(|(_, action)| *action).collect();
        actions.retain(survives_an_application);
        assert!(
            actions.is_empty(),
            "every plain press on this pad belongs to the application in front"
        );
        for action in [Action::Guide, Action::Keyboard, Action::Screenshot] {
            assert!(survives_an_application(&action), "{action:?}");
        }
    }

    /// A pad with no GilRs behind it at all still reads as centred rather than
    /// panicking — the Steam Controller is exactly that case, because GilRs
    /// enumerates nothing for it.
    #[test]
    fn sticks_read_without_gilrs_are_at_rest() {
        let sticks = Sticks::read(None);
        assert_eq!(sticks.left, (0.0, 0.0));
        assert_eq!(sticks.right, (0.0, 0.0));
        assert_eq!(sticks.dpad, [false; Direction::COUNT]);
    }
}
