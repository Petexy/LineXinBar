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
//! That driver also makes the gamepad the kernel never did, so that the pad
//! works in a game and not only in this menu. Which puts a third copy of it on
//! the machine, and the shell must not read that one: it would be every press
//! arriving twice, once exactly and once through a mapping database that has
//! never heard of this pad. So GilRs is asked to skip it wherever GilRs is
//! read — see [`is_a_stand_in`].
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
    /// Empty unless one of the triggers the pointer clicks with moved.
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

/// Owns the platform controller context and translates it into lattice actions.
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
    /// Whether each trigger was down last poll, as `(left, right)`, for the
    /// reason the D-pad's state above it is kept: a trigger is read as a
    /// *position* on every pad here, so the press and the release are this
    /// shell's to find rather than something a device delivers.
    triggers_held: (bool, bool),
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
    /// The pad a thumb was last on.
    ///
    /// Which controller a person is *using* is not a question a machine with
    /// four of them plugged in can answer any other way, and something has to:
    /// an emulator gives one pad to player one, and the pad in somebody's hands
    /// is the one that should be it. See [`ControllerInput::in_hand`].
    last_touched: Option<Touched>,
}

/// Which controller the last thumb was on.
///
/// Two arms because the Steam Controller is read from its report rather than
/// through GilRs, and so has no `GamepadId` to be named by — see the module
/// note. It still has to be nameable: the pad an emulator binds to player one
/// is the stand-in this shell makes for it, and a hand on the controller is a
/// hand on that.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Touched {
    Pad(gilrs::GamepadId),
    SteamController,
}

impl ControllerInput {
    pub fn new(enabled: bool) -> Self {
        if !enabled {
            tracing::info!("game-controller input disabled");
            return Self {
                gilrs: None,
                navigation: Navigation::default(),
                dpad_held: [false; Direction::COUNT],
                triggers_held: (false, false),
                guide_chorded: false,
                last_touched: None,
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
                    triggers_held: (false, false),
                    guide_chorded: false,
                    last_touched: None,
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
                    triggers_held: (false, false),
                    guide_chorded: false,
                    last_touched: None,
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
    /// The guide button is the deliberate exception, along with the three
    /// chords spelled on it and on Select.  Controllers are read straight from
    /// `/dev/input` rather than through Wayland, so they reach the shell even
    /// while a game holds the keyboard — which is the only reason a user can
    /// get back out of that game at all, photograph it, or reach Valve's own
    /// overlay over it.
    ///
    /// The right stick and the two triggers are read on the same terms and for
    /// the same reason: the pointer they drive and click with is wanted
    /// *inside* the application, which is exactly when nothing else here is
    /// listened to.
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
        let pad = self.pad.poll();
        if let Some(frame) = &pad {
            stirred |= !frame.held.is_empty();
            // Which controller the hand is on, for the same reason a GilRs
            // press says so below: the pad somebody has picked up is the pad an
            // emulator should give player one. Held rather than pressed, and
            // the sticks with it, because a thumb resting on a direction is a
            // hand on the controller too.
            if !frame.held.is_empty()
                || stick_is_pushed(frame.left_stick)
                || stick_is_pushed(frame.right_stick)
            {
                self.last_touched = Some(Touched::SteamController);
            }
            actions.extend(pad_actions(frame, &mut chorded));
            if !frame.pressed.is_empty() {
                tracing::debug!(pressed = ?frame.pressed, "steam controller buttons");
            }
        }

        if let Some(gilrs) = self.gilrs.as_mut() {
            while let Some(event) = gilrs.next_event() {
                // The stand-in this shell makes for the Steam Controller, which
                // it has already read above and exactly. Drained rather than
                // skipped outright: GilRs' cached state and hot-plug list have
                // to stay current for a device it will keep being offered.
                if is_a_stand_in(&gilrs.gamepad(event.id)) {
                    continue;
                }
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
                        // has no use for is still a thumb on the pad — and it
                        // is a thumb on *this* pad, which is the one a game
                        // started from here should answer to.
                        stirred = true;
                        self.last_touched = Some(Touched::Pad(event.id));
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
                                let held = guide_held || guide_is_held(gilrs);
                                steam_overlay_chord(button, code, held)
                                    .or_else(|| photograph_chord(button, code, held))
                            })
                            .or_else(|| action_for_button(button, code, layout));
                        // Both chords spelled on the guide button spend its
                        // hold: the button was pressed for the chord, and
                        // letting go of it must not open the guide as well.
                        if matches!(action, Some(Action::Screenshot | Action::SteamOverlay)) {
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
                        // The guide button, and nothing else: the shell acts on
                        // presses, and a menu row activated on the way back up
                        // would fire twice. The pointer's own two buttons are
                        // not here either — they are the triggers, and a
                        // trigger is read as a position rather than as a press.
                        // See [`Sticks::read`].
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
        // And the two mouse buttons, which are the two triggers — read as a
        // position like the sticks beside them, and turned into the edges a
        // button has here. One road for every pad on the machine: see
        // [`ControllerInput::trigger_edges`].
        let pulled = self.trigger_edges(sticks.triggers);
        if !pulled.is_empty() {
            // Logged for the reason every button press is: a trigger that has
            // become a mouse button and does nothing is otherwise
            // indistinguishable from a controller nothing is reading, and
            // this is the only line that can tell the two apart.
            tracing::debug!(?pulled, travel = ?sticks.triggers, "the pointer's triggers");
        }
        clicks.extend(pulled);
        // Held rather than pressed, for both the D-pad and the sticks: this is
        // asked of every poll and a thumb resting on a direction is a hand on
        // the pad on all of them, not only the one it landed on.
        stirred |= sticks.dpad.iter().any(|held| *held)
            || stick_is_pushed(sticks.left)
            || stick_is_pushed(sticks.right)
            // A finger on a trigger is a hand on the pad whether or not it has
            // gone far enough to be a click, which is the wider question this
            // asks. Every pad at once, since the Steam Controller's own
            // reading has been folded in above.
            || sticks.triggers.0 > TRIGGER_TOUCHED
            || sticks.triggers.1 > TRIGGER_TOUCHED;
        // And which pad it is on. Buttons say so as they arrive; a stick is
        // read rather than delivered, so the pad it is on has to be looked for
        // — and it has to be, because a bar walked with the stick alone would
        // otherwise never say which controller was doing the walking.
        if let Some(touched) = self.gilrs.as_ref().and_then(pushed_pad) {
            self.last_touched = Some(Touched::Pad(touched));
        }
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

    /// The pad somebody is holding, as much of it as GilRs will say.
    ///
    /// The one last touched, or — where nothing has been touched yet this
    /// session — the first pad on the machine, which is the best guess there
    /// is and is right on the machine that has one. `None` where there is no
    /// controller at all.
    ///
    /// What it is for is telling a game which of several controllers is the
    /// one in front of the television. See [`crate::pads::order`].
    pub fn in_hand(&self) -> Option<crate::pads::InHand> {
        // The Steam Controller first, and outside GilRs, because that is where
        // it is read: the device an emulator will bind is the stand-in this
        // shell makes for it, and this names that. `None` where there is no
        // stand-in — a pad only this shell can read is not a pad to give
        // anybody player one.
        if self.last_touched == Some(Touched::SteamController) {
            if let Some(stand_in) = crate::steam_hid::stand_in() {
                return Some(crate::pads::InHand {
                    name: stand_in.name,
                    vendor: Some(stand_in.vendor),
                    product: Some(stand_in.product),
                });
            }
        }

        let gilrs = self.gilrs.as_ref()?;
        let named = |gamepad: gilrs::Gamepad<'_>| crate::pads::InHand {
            // The name the *operating system* gave it, not the one a mapping
            // database did: what this is compared against is the kernel's own
            // list of devices, and the two names are not always the same.
            name: gamepad.os_name().to_string(),
            vendor: gamepad.vendor_id(),
            product: gamepad.product_id(),
        };
        if let Some(Touched::Pad(id)) = self.last_touched {
            let gamepad = gilrs.gamepad(id);
            if gamepad.is_connected() {
                return Some(named(gamepad));
            }
        }
        gilrs.gamepads().next().map(|(_, gamepad)| named(gamepad))
    }

    /// Where that pad's controls are, as a mapping database has them.
    ///
    /// What this is for is telling an emulator which button is which — see
    /// [`crate::retroarch::controllers`]. The emulator will guess for itself
    /// if nobody tells it, and its guess is a list of pads it has heard of;
    /// this shell has a *different* list, kept by SDL and carried by GilRs, and
    /// the two lists do not have the same pads on them.
    ///
    /// `None` unless the database really knows this pad. GilRs answers for one
    /// it does not by falling back to the kernel's positional names, which are
    /// a coin toss on the two middle face buttons — see [`Layout`] — and a
    /// coin toss written into an emulator's settings is worse than leaving it
    /// to guess, because the emulator's guess at least gets the pads on *its*
    /// list right.
    ///
    /// Controls the device itself never said it had are left out. A database
    /// names a D-pad by the buttons it would be if it were buttons, whatever
    /// the pad actually sends, so some of what comes back names nothing.
    pub fn mapping(&self, pad: &crate::pads::Pad) -> Option<crate::pads::Mapping> {
        use crate::pads::Control;

        let gilrs = self.gilrs.as_ref()?;
        let same = |gamepad: &gilrs::Gamepad<'_>| {
            if gamepad.os_name() != pad.name {
                return false;
            }
            match (gamepad.vendor_id(), gamepad.product_id()) {
                (Some(vendor), Some(product)) => vendor == pad.vendor && product == pad.product,
                _ => true,
            }
        };
        let Some((_, gamepad)) = gilrs.gamepads().find(|(_, gamepad)| same(gamepad)) else {
            return crate::pads::xinput(pad);
        };
        if Layout::of(gilrs, gamepad.id()) != Layout::Mapped {
            // GilRs will answer for a pad no database knows by reading the
            // codes back at us under the kernel's own names for them, and on a
            // controller of Xbox's shape two of those names are the wrong way
            // round — see [`crate::pads::xinput`], which works the same pad out
            // from what it declares and gets those two right.
            return crate::pads::xinput(pad);
        }

        let buttons = [
            (Control::South, Button::South),
            (Control::East, Button::East),
            (Control::North, Button::North),
            (Control::West, Button::West),
            (Control::LeftBumper, Button::LeftTrigger),
            (Control::RightBumper, Button::RightTrigger),
            (Control::LeftTrigger, Button::LeftTrigger2),
            (Control::RightTrigger, Button::RightTrigger2),
            (Control::Select, Button::Select),
            (Control::Start, Button::Start),
            (Control::LeftStick, Button::LeftThumb),
            (Control::RightStick, Button::RightThumb),
            (Control::DPadUp, Button::DPadUp),
            (Control::DPadDown, Button::DPadDown),
            (Control::DPadLeft, Button::DPadLeft),
            (Control::DPadRight, Button::DPadRight),
        ];
        let axes = [
            (Control::LeftX, Axis::LeftStickX),
            (Control::LeftY, Axis::LeftStickY),
            (Control::RightX, Axis::RightStickX),
            (Control::RightY, Axis::RightStickY),
            (Control::DPadX, Axis::DPadX),
            (Control::DPadY, Axis::DPadY),
        ];
        // The guide button is not among them, on purpose. It is the way out of
        // whatever is in front, it is held back from every application the
        // shell starts, and handing an emulator a binding for it would be
        // handing over the one button that is not an application's to have.
        let mut at = Vec::new();
        for (control, button) in buttons {
            if let Some(code) = gamepad.button_code(button).and_then(where_it_is) {
                at.push((control, code));
            }
        }
        for (control, axis) in axes {
            if let Some(code) = gamepad.axis_code(axis).and_then(where_it_is) {
                at.push((control, code));
            }
        }
        at.retain(|(_, code)| pad.has(*code));
        // Nothing left is the same answer as nothing known: it is a pad whose
        // database entry names controls this device does not have, and there
        // is no line worth writing for it.
        if at.is_empty() {
            return None;
        }
        Some(crate::pads::Mapping::new(at))
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

    /// Which mouse buttons the triggers pressed or let go of since the last
    /// poll, given how far each is pulled.
    ///
    /// The same job [`Self::arrow_edges`] does for the D-pad, and here for a
    /// stronger reason: a trigger is not a button on every pad. Some report
    /// one, some report only an axis, and the Steam Controller reports a
    /// number out of its own HID report — so the edges are found here, once,
    /// from how far the thing is pulled. That is what makes a trigger the same
    /// mouse button on every controller somebody might pick up. See
    /// [`trigger_travel`], which is where the three roads meet.
    fn trigger_edges(&mut self, pulled: (f32, f32)) -> Vec<(u32, bool)> {
        let mut edges = Vec::new();
        for (pulled, was_down, button) in [
            (pulled.1, &mut self.triggers_held.1, RIGHT_TRIGGER_CLICKS),
            (pulled.0, &mut self.triggers_held.0, LEFT_TRIGGER_CLICKS),
        ] {
            let down = trigger_is_down(pulled, *was_down);
            if down != *was_down {
                *was_down = down;
                edges.push((button, down));
            }
        }
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
    /// How far each analogue trigger is pulled, as `(left, right)` in
    /// 0.0..=1.0 — the pointer's two mouse buttons, before anything has
    /// decided that a pull is a press. See [`trigger_is_down`].
    triggers: (f32, f32),
}

/// Which pad has a stick or a D-pad pushed, if any has.
///
/// The first one found: two hands on two pads is not a thing this has to
/// resolve, and the shell has been asking "is anybody holding anything" with
/// one answer for as long as it has asked at all.
fn pushed_pad(gilrs: &Gilrs) -> Option<gilrs::GamepadId> {
    gilrs.gamepads().find_map(|(id, gamepad)| {
        if is_a_stand_in(&gamepad) {
            return None;
        }
        let pushed = stick_is_pushed((
            gamepad.value(Axis::LeftStickX),
            gamepad.value(Axis::LeftStickY),
        )) || stick_is_pushed((
            gamepad.value(Axis::RightStickX),
            gamepad.value(Axis::RightStickY),
        )) || gamepad.is_pressed(Button::DPadLeft)
            || gamepad.is_pressed(Button::DPadRight)
            || gamepad.is_pressed(Button::DPadUp)
            || gamepad.is_pressed(Button::DPadDown);
        pushed.then_some(id)
    })
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

        self.triggers.0 = self.triggers.0.max(frame.triggers.0);
        self.triggers.1 = self.triggers.1.max(frame.triggers.1);
    }

    fn read(gilrs: Option<&Gilrs>) -> Self {
        let mut dpad = [false; Direction::COUNT];
        let (mut lx, mut ly) = (0.0_f32, 0.0_f32);
        let (mut rx, mut ry) = (0.0_f32, 0.0_f32);
        let (mut lt, mut rt) = (0.0_f32, 0.0_f32);

        let Some(gilrs) = gilrs else {
            return Self {
                left: (lx, ly),
                right: (rx, ry),
                dpad,
                triggers: (lt, rt),
            };
        };

        // The shoulder bumpers deliberately do not appear here: they move
        // between displays, which is a press rather than something that
        // repeats while held.
        for (_, gamepad) in gilrs.gamepads() {
            // Read from its report instead, and merged in by
            // [`Sticks::merge_pad`]. See the module note.
            if is_a_stand_in(&gamepad) {
                continue;
            }
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

            lt = lt.max(trigger_travel(&gamepad, Button::LeftTrigger2, Axis::LeftZ));
            rt = rt.max(trigger_travel(
                &gamepad,
                Button::RightTrigger2,
                Axis::RightZ,
            ));
        }

        Self {
            left: (lx, ly),
            right: (rx, ry),
            dpad,
            triggers: (lt, rt),
        }
    }
}

/// How far one trigger is pulled, 0.0..=1.0, whichever way this pad reports it.
///
/// Two ways, because the hardware has two. A pad the mapping database knows —
/// nearly every controller somebody owns — has its triggers named as buttons
/// with a value, and that value is the pull. A pad nothing has heard of has
/// only the kernel's own naming, and there an analogue trigger is an *axis*:
/// `ABS_Z` and `ABS_RZ`, which arrive as `LeftZ` and `RightZ` and never as a
/// button at all. Reading only the first left the mouse buttons missing on
/// exactly the pads this module takes the most trouble over — measured, on a
/// pad made for the purpose.
///
/// An axis is rescaled because GilRs hands back the whole of it: a trigger
/// resting at its stop reads −1 and pulled all the way reads 1, so the travel
/// is the half of that range above rest. A pad whose `ABS_Z` is not a trigger
/// at all — a flight stick's twist — sits at the middle of its range and so
/// reads half pulled, which is short of [`TRIGGER_CLICKS`] and clicks nothing.
fn trigger_travel(gamepad: &gilrs::Gamepad<'_>, button: Button, axis: Axis) -> f32 {
    if let Some(data) = gamepad.button_data(button) {
        return data.value();
    }
    match gamepad.axis_data(axis) {
        Some(data) => (data.value() + 1.0) / 2.0,
        None => 0.0,
    }
}

/// Whether a controller GilRs is offering is the gamepad this shell makes for
/// the Steam Controller, which the shell reads from that pad's report instead.
///
/// Not a rule about ignoring a controller — the pad works, and everything else
/// on the machine reads exactly this device. It is a rule about reading one pad
/// once. See [`crate::steam_hid::is_a_stand_in`], which owns the answer.
fn is_a_stand_in(gamepad: &gilrs::Gamepad<'_>) -> bool {
    crate::steam_hid::is_a_stand_in(gamepad.vendor_id(), gamepad.product_id())
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
/// Absent on purpose: `X`, which is who is on Steam on its own and the keyboard
/// chord with `View` held — one button whose meaning a modifier changes, and a
/// table lookup cannot see a modifier; `View` itself, which is only that
/// chord's modifier and the
/// overlay chord's other half; `Steam`, which acts on its release instead so
/// the two chords spelled on it can claim the hold (see [`is_guide`]); the left
/// stick press, which means nothing to this shell at all; and the whole D-pad,
/// which goes through [`Navigation`] instead so that holding a direction
/// repeats at the same rate every other pad's does. Listed here as well it
/// would walk the menu two rows per press.
///
/// The triggers are not here either, and they are not buttons in the first
/// place: this pad reports them as travel, and what the shell makes of that is
/// the pointer's two clicks. See [`RIGHT_TRIGGER_CLICKS`] and [`Sticks::read`].
const PAD_ACTIONS: &[(Buttons, Action)] = &[
    (Buttons::A, Action::Launch),
    (Buttons::B, Action::Back),
    (Buttons::Y, Action::Menu),
    // The pad's own Start, and the same button as every other pad's: Accept,
    // except over the on-screen keyboard. See [`Action::Submit`].
    (Buttons::MENU, Action::Submit),
    (Buttons::L1, Action::PrevScreen),
    (Buttons::R1, Action::NextScreen),
    // The right stick pressed, which is the videos floating over the guide. See
    // [`Action::Floating`].
    (Buttons::R3, Action::Floating),
];

/// What one frame of the Steam Controller is worth to the shell.
///
/// `chorded` is the guide button's hold, carried between polls: whether the
/// button now down has already been spent on a chord and so must not open the
/// overlay when it comes back up.
///
/// Separate from [`ControllerInput::poll`] because it is the one part of
/// reading this pad that can be exercised without the pad: everything else
/// there is a device to open and a report to be handed. Three chords and an
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
    // And Valve's overlay, spelled as Steam with View. `VIEW` is absent from
    // [`PAD_ACTIONS`] already, as the keyboard chord's modifier, so this costs
    // the pad nothing it was doing.
    let overlay = frame.held.has(Buttons::STEAM) && frame.pressed.has(Buttons::VIEW);
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
    if overlay {
        actions.push(Action::SteamOverlay);
        *chorded = true;
    }
    // The guide button acts on the way back up rather than the way down, so the
    // chords above can claim it; `STEAM` is absent from `PAD_ACTIONS` for that
    // reason. See [`is_guide`].
    if frame.released.has(Buttons::STEAM) {
        if !*chorded {
            actions.push(Action::Guide);
        }
        *chorded = false;
    }
    // The left-hand face button, which is the one control on this pad a
    // modifier changes the meaning of: `View` held with it is the keyboard
    // chord — the same two controls as everywhere else — and on its own it is
    // the friends panel, as `X` is on every mapped pad. That is the whole of
    // why it is absent from `PAD_ACTIONS`: a row there would spend the press
    // before this could read what was held with it.
    if frame.pressed.has(Buttons::X) {
        actions.push(if frame.held.has(Buttons::VIEW) {
            Action::Keyboard
        } else {
            Action::Friends
        });
    }
    actions
}

/// Which mouse button each trigger is.
///
/// Two controls, one each: the right trigger is the left button and the left
/// trigger is the right one. That is the handheld's own arrangement — a Steam
/// Deck in its desktop mode clicks with `R2` and opens a context menu with
/// `L2` — and somebody who has held one of those reaches for it without being
/// told. It also puts the two mouse buttons under the two fingers that are not
/// doing anything else while a thumb is aiming.
///
/// The triggers rather than the face buttons, which is what these used to be,
/// for a reason worth more than the familiarity: `A` and `B` are the on-screen
/// keyboard's. The board is driven with `A` on its keys and `B` to put it
/// away, so a pointer that borrowed them had to give them back for as long as
/// it was up — and the one thing a user wants a mouse for while a keyboard is
/// on screen is to click the field they are about to type into. With the
/// clicks on the triggers, nothing has to be handed over: the board keeps
/// every control it was ever driven with, and the pointer keeps both buttons.
///
/// Nothing else on the pad is taken. The bumpers still move between displays,
/// the stick presses are the floating video's, and the left-hand face button
/// is still half of the keyboard chord.
const RIGHT_TRIGGER_CLICKS: u32 = BTN_LEFT;
/// See [`RIGHT_TRIGGER_CLICKS`].
const LEFT_TRIGGER_CLICKS: u32 = BTN_RIGHT;

/// How far a trigger has to be pulled before it is a click, and how far it has
/// to come back before it is not.
///
/// GilRs' own numbers, and they are here so that the pad this shell reads
/// itself agrees with the pads GilRs reads: a trigger that clicked at half its
/// travel on one controller and three quarters on another would be two
/// different buttons. The gap between the two is what stops a finger held at
/// the threshold rattling the mouse button several hundred times a second.
const TRIGGER_CLICKS: f32 = 0.75;
/// See [`TRIGGER_CLICKS`].
const TRIGGER_LETS_GO: f32 = 0.65;

/// How far a trigger has to be pulled to count as a finger on it at all.
///
/// A different and much easier question than whether it is a click, and asked
/// for a different reason: what this decides is whether somebody has the
/// controller in their hands, which is true long before a trigger bottoms out.
/// Far enough off rest that a worn trigger resting a little open does not
/// answer yes for as long as the pad stays plugged in — the same worry
/// [`stick_is_pushed`] has, about the same hardware.
const TRIGGER_TOUCHED: f32 = 0.2;

/// Whether a trigger pulled this far is down, given whether it was down.
///
/// The hysteresis of [`TRIGGER_CLICKS`], as a rule on its own: past the first
/// number it is down, below the second it is up, and in between it is whatever
/// it already was.
fn trigger_is_down(pulled: f32, was_down: bool) -> bool {
    if was_down {
        pulled > TRIGGER_LETS_GO
    } else {
        pulled >= TRIGGER_CLICKS
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
/// could show the keyboard but never hide it. The menu has its own button
/// under every spelling there is — the pad's, `Home`, `Super`, the mouse's
/// side button, and the compositor's binding behind them — and does not need
/// another that costs the keyboard its off switch.
fn chord_action(button: Button, code: u32, layout: Layout, select_held: bool) -> Option<Action> {
    if !select_held {
        return None;
    }
    is_left_face(button, code, layout).then_some(Action::Keyboard)
}

/// Whether an action still counts once an application owns the screen.
///
/// Four of them do, and everything else is meant for the game rather than for
/// the shell behind it: the way back out of the application, the way to type
/// into it, the way to photograph it, and the way to reach Valve's overlay over
/// it. All four are read straight from `/dev/input` rather than through
/// Wayland, which is the only reason any of them arrives at all while the game
/// holds the keyboard — and a picture is nearly always wanted of the game
/// rather than of the bar, so this is the one case that matters most for the
/// third of them. The fourth means nothing anywhere else: it is a chord about
/// the game in front, and with no game in front it is spent on nothing.
fn survives_an_application(action: &Action) -> bool {
    matches!(
        action,
        Action::Guide | Action::Keyboard | Action::Screenshot | Action::SteamOverlay
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

/// Whether a press, with the guide button held, is the Steam overlay chord.
///
/// The guide button and Select: `STEAM`+`View` on a Steam Controller or a Deck,
/// the PlayStation button and Create on a DualSense, Guide and View on an Xbox
/// pad. The middle two buttons of the pad, side by side, and reachable with one
/// thumb — which matters here more than it does for the screenshot, because
/// this is a chord pressed *while playing* rather than while looking at
/// something.
///
/// It is spelled on the guide button because of what it is for. This shell
/// takes that button for itself (see [`is_guide`]), and on every other machine
/// shaped like a console it is the button Steam's overlay comes up on. Taking a
/// control away and offering nothing in its place would be this shell deciding
/// that nobody may reach Steam's friends list, its browser or its guides while
/// a game is running. So the button stays the shell's and the overlay is a
/// chord on it: the same button, one more thumb.
///
/// Select rather than another face button for the reason Select is the
/// keyboard chord's modifier: it is the one control on a pad that no game
/// wants. Here it is the chord's *other half* rather than its modifier, which
/// is not a contradiction — a chord is two buttons, and which of them is held
/// first is decided by which of them the shell has to answer on its release.
/// The guide button does, so the guide button is the one held.
fn steam_overlay_chord(button: Button, code: u32, guide_held: bool) -> Option<Action> {
    if !guide_held {
        return None;
    }
    is_select(button, code).then_some(Action::SteamOverlay)
}

/// Whether a press is Select — the small button left of centre, called View on
/// an Xbox pad, Create on a DualSense and View on a Steam Controller.
///
/// Named the two ways every other button here is, and for the same reason the
/// modifier in [`select_is_held`] is read both ways: a pad the mapping database
/// has never heard of still has the button, and without the raw code the chord
/// could not be spelled on it at all.
fn is_select(button: Button, code: u32) -> bool {
    match button {
        Button::Select => true,
        Button::Unknown => code == evdev::BTN_SELECT,
        _ => false,
    }
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
/// It is the modifier of [`photograph_chord`] and of [`steam_overlay_chord`],
/// and a modifier that also did something on the way down could not be one: the
/// guide would already be up by the time the bumper arrived, and the picture
/// would be of the guide rather than of whatever the user wanted a picture of.
/// So the press only begins a hold, and letting go is what opens or closes the
/// guide — unless the hold was spent on a chord, in which case nothing happens
/// at all and the button has done the one job it was pressed for.
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

/// A code GilRs gives for a control, as the kernel's own key or axis.
///
/// GilRs carries the event type in the top half of the number — see
/// [`gilrs::ev::Code`] — and nothing else is a control an emulator can bind.
fn where_it_is(code: gilrs::ev::Code) -> Option<crate::pads::At> {
    let raw = code.into_u32();
    let (kind, code) = ((raw >> 16) as u16, u16::try_from(raw & 0xffff).ok()?);
    match kind {
        EV_KEY => Some(crate::pads::At::Key(code)),
        EV_ABS => Some(crate::pads::At::Axis(code)),
        _ => None,
    }
}

/// The kernel's two event types a controller's controls come as.
const EV_KEY: u16 = 0x01;
const EV_ABS: u16 = 0x03;

/// Whether a press is the left-hand face button — the one with `X` printed on
/// it, Square on a PlayStation pad, `Y` on a Nintendo one.
///
/// Who is on Steam, and the keyboard chord's other half while Select is held.
/// See [`Action::Friends`] and [`chord_action`].
fn is_left_face(button: Button, code: u32, layout: Layout) -> bool {
    match layout {
        Layout::Mapped => matches!(button, Button::West),
        Layout::Guessed => is_middle_face(button, code, Face::Left),
    }
}

/// Whether a press is the *top* face button — Y on an Xbox pad, Triangle on a
/// PlayStation one — which raises the context menu, the way Triangle has opened
/// the options menu on a console shell of this shape since the first one.
fn is_top_face(button: Button, code: u32, layout: Layout) -> bool {
    match layout {
        Layout::Mapped => matches!(button, Button::North),
        Layout::Guessed => is_middle_face(button, code, Face::Top),
    }
}

/// Which of the two middle face buttons a guess is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Face {
    /// The left-hand one: the friends list, and the keyboard chord.
    Left,
    /// The top one: the context menu.
    Top,
}

/// Whether a press is the named middle face button on a pad nothing knows the
/// layout of.
///
/// These two are the one place GilRs' namings disagree, and there is no third
/// source to ask: `0x133` is `BTN_X` under the kernel's legacy gamepad names
/// and `BTN_NORTH` under its positional ones, and `0x134` is the same
/// disagreement the other way round. So the code is taken at the *legacy*
/// names' word — which is what xpad and every driver modelled on it send —
/// and the pair is split between the two jobs.
///
/// **Split, rather than both buttons doing both.** Both used to be taken to be
/// both, and that was right for as long as the only thing either one did was
/// raise the context menu: a wrong guess then cost nothing, because both
/// guesses led to the same screen. Two different screens cannot share a button
/// that way — whichever was asked first would answer every press, and the
/// friends panel would be unreachable on exactly the pads the context menu used
/// to be. The guess is right on every pad modelled on xpad, wrong on a pad that
/// numbers the pair the other way *and* is missing from the SDL database, and
/// there is nothing else to ask. Select still separates the keyboard chord from
/// the press underneath it, which is why the left-hand button can be two things
/// and the top one only ever the menu.
fn is_middle_face(button: Button, code: u32, face: Face) -> bool {
    // Either of GilRs' two namings for the pair, or a pad it could name nothing
    // on at all — one presenting as a plain joystick, whose buttons run from
    // `BTN_TRIGGER` and whose third and fourth are where the face pair sits.
    // The same ordering [`action_for_button`] already reads the first two
    // under, and a gamepad GilRs *did* name never sends those codes.
    if !matches!(button, Button::North | Button::West | Button::Unknown) {
        return false;
    }
    match face {
        Face::Left => matches!(code, evdev::BTN_X | evdev::BTN_THUMB2),
        Face::Top => matches!(code, evdev::BTN_WEST | evdev::BTN_TOP),
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
    /// The right stick pressed, R3 — the videos floating over the guide. The
    /// left one is not here because it means nothing to this shell: L3 is the
    /// application's, whatever the application is.
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
    // And the left-hand one, which is who is on Steam. Reached only once the
    // chord in front of this has declined the press — see [`chord_action`],
    // which is asked first precisely so that Select held with this button is
    // still the keyboard rather than the panel.
    if is_left_face(button, code, layout) {
        return Some(Action::Friends);
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
        // The right stick pressed: the videos floating over the guide. See
        // [`Action::Floating`]. Nothing the pointer wants — its own two
        // buttons are the triggers, which reach nothing in the shell at all.
        Button::RightThumb => return Some(Action::Floating),
        Button::Unknown => {}
        _ => return None,
    }

    match code {
        evdev::BTN_SOUTH | evdev::BTN_TRIGGER => Some(Action::Launch),
        evdev::BTN_START => Some(Action::Submit),
        evdev::BTN_EAST | evdev::BTN_THUMB => Some(Action::Back),
        evdev::BTN_TL => Some(Action::PrevScreen),
        evdev::BTN_TR => Some(Action::NextScreen),
        evdev::BTN_THUMBR => Some(Action::Floating),
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
    }

    /// The bug this was written for: the context menu could not be raised from
    /// an 8BitDo — or from any other pad GilRs found no SDL mapping for. Both
    /// middle face buttons were given to the keyboard chord and the menu was
    /// left with none at all.
    ///
    /// What the fix for that did — give both buttons to both jobs — lasted
    /// only as long as there was one job. The left-hand button is the friends
    /// panel now, so the pair is split by the codes the legacy gamepad names
    /// give them: `0x133` is the left-hand button and `0x134` is the top one,
    /// which is right on every pad modelled on xpad and is the only guess there
    /// is anything to base.
    #[test]
    fn an_unmapped_pads_middle_face_buttons_are_split_between_the_two_jobs() {
        // Whichever of its two namings GilRs reached for, the code decides.
        for button in [Button::North, Button::West, Button::Unknown] {
            assert_eq!(
                action_for_button(button, evdev::BTN_X, Layout::Guessed),
                Some(Action::Friends),
                "{button:?} 0x133"
            );
            assert_eq!(
                action_for_button(button, evdev::BTN_WEST, Layout::Guessed),
                Some(Action::Menu),
                "{button:?} 0x134"
            );
            // And the left-hand one held with Select is still the keyboard,
            // which is what it was worth before it was worth anything alone.
            assert_eq!(
                chord_action(button, evdev::BTN_X, Layout::Guessed, true),
                Some(Action::Keyboard),
                "{button:?} 0x133"
            );
            // The top one never is. Select + the context menu means nothing,
            // and a chord that answered it would cost the board its off
            // switch — see [`chord_action`].
            assert_eq!(
                chord_action(button, evdev::BTN_WEST, Layout::Guessed, true),
                None,
                "{button:?} 0x134"
            );
        }

        // A pad presenting as a plain joystick has no gamepad codes for GilRs
        // to name anything from, and its face buttons run from `BTN_TRIGGER`.
        // The third and fourth are the same pair, split the same way.
        assert_eq!(
            action_for_button(Button::Unknown, evdev::BTN_THUMB2, Layout::Guessed),
            Some(Action::Friends)
        );
        assert_eq!(
            action_for_button(Button::Unknown, evdev::BTN_TOP, Layout::Guessed),
            Some(Action::Menu)
        );
        // But not the two below them, which are already A and B.
        for code in [evdev::BTN_TRIGGER, evdev::BTN_THUMB] {
            assert!(
                !matches!(
                    action_for_button(Button::Unknown, code, Layout::Guessed),
                    Some(Action::Menu | Action::Friends)
                ),
                "{code:#x}"
            );
        }
    }

    /// A mapped pad is read by name alone, so the guesswork above can never
    /// reach it: `X` is the friends panel and only `Y` raises the menu.
    #[test]
    fn a_mapped_pads_left_face_button_is_never_the_context_menu() {
        assert!(is_left_face(Button::West, evdev::BTN_X, Layout::Mapped));
        assert!(!is_top_face(Button::West, evdev::BTN_X, Layout::Mapped));
        assert_eq!(
            action_for_button(Button::West, evdev::BTN_X, Layout::Mapped),
            Some(Action::Friends),
            "X alone is who is on Steam"
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
        // On its own the left-hand face button is not the board — it is the
        // friends panel, which is the press the chord has to be told apart
        // from. A board that came up whenever somebody asked who was on Steam
        // would be the modifier counting for nothing.
        assert_eq!(
            chord_action(Button::West, evdev::BTN_X, Layout::Mapped, false),
            None
        );
        assert_eq!(
            action_for_button(Button::West, evdev::BTN_X, Layout::Mapped),
            Some(Action::Friends)
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

        // With no mapping and no name the code is all there is, and it is read
        // at the legacy gamepad names' word: `0x133` is the left-hand button
        // and `0x134` is the top one. Both used to be taken here, which was
        // affordable only while the two buttons wanted the same screen.
        assert!(is_left_face(Button::Unknown, evdev::BTN_X, Layout::Guessed));
        assert!(!is_left_face(
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

    /// Steam's own overlay, on the same modifier as the screenshot and with the
    /// button beside it: the shell took the guide button, and this is what it
    /// gives back.
    #[test]
    fn the_guide_button_with_select_asks_for_steams_overlay() {
        assert_eq!(
            steam_overlay_chord(Button::Select, 0, true),
            Some(Action::SteamOverlay)
        );
        assert_eq!(
            steam_overlay_chord(Button::Unknown, evdev::BTN_SELECT, true),
            Some(Action::SteamOverlay)
        );

        // Select on its own is still nothing — it is the keyboard chord's
        // modifier, and a modifier that also did something could not be one.
        assert_eq!(steam_overlay_chord(Button::Select, 0, false), None);
        assert_eq!(
            action_for_button(Button::Select, evdev::BTN_SELECT, Layout::Mapped),
            None
        );

        // And the two chords on the guide button do not overlap: the bumper is
        // the picture, Select is the overlay, and neither is the other.
        assert_eq!(steam_overlay_chord(Button::RightTrigger, 0, true), None);
        assert_eq!(photograph_chord(Button::Select, 0, true), None);

        // Nothing else spells it, `Start` least of all: its raw code is the one
        // next door to Select's, and a pad the database cannot name is read by
        // that code alone.
        for (button, code) in [
            (Button::Start, evdev::BTN_START),
            (Button::Mode, evdev::BTN_MODE),
            (Button::South, evdev::BTN_SOUTH),
            (Button::Unknown, evdev::BTN_START),
            (Button::Unknown, evdev::BTN_MODE),
        ] {
            assert_eq!(steam_overlay_chord(button, code, true), None, "{button:?}");
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
        // The four actions an inactive shell still acts on. Everything else
        // is meant for the game that has the screen; these are the way out of
        // it, the way to type into it, the way to photograph it, and the way to
        // reach Valve's own overlay over it.
        let mut actions = vec![
            Action::Guide,
            Action::Keyboard,
            Action::Screenshot,
            Action::SteamOverlay,
            Action::Launch,
            Action::Left,
            Action::Back,
        ];
        actions.retain(survives_an_application);
        assert_eq!(
            actions,
            [
                Action::Guide,
                Action::Keyboard,
                Action::Screenshot,
                Action::SteamOverlay,
            ]
        );
    }

    /// The two triggers click, one mouse button each, and the pointer takes
    /// nothing else on the pad.
    #[test]
    fn the_pointer_clicks_with_the_triggers() {
        assert_eq!(RIGHT_TRIGGER_CLICKS, BTN_LEFT);
        assert_eq!(LEFT_TRIGGER_CLICKS, BTN_RIGHT);

        // Nothing else on the pad is a click: every button below is either the
        // shell's own or the running application's, and none of them reaches
        // the pointer at all now that the clicks are read off the triggers.
        let mut input = ControllerInput::new(false);
        assert!(input.trigger_edges((0.0, 0.0)).is_empty());
    }

    /// A trigger is a click at the depth GilRs calls a press, and it stays one
    /// until the finger has come a good way back: a finger resting exactly at
    /// the threshold must not rattle the mouse button.
    #[test]
    fn a_trigger_clicks_at_the_same_depth_on_every_pad() {
        // Coming down: nothing until the pull reaches the threshold.
        assert!(!trigger_is_down(0.0, false));
        assert!(!trigger_is_down(0.5, false));
        assert!(!trigger_is_down(TRIGGER_CLICKS - 0.01, false));
        assert!(trigger_is_down(TRIGGER_CLICKS, false));
        assert!(trigger_is_down(1.0, false));

        // And going back up: held through the gap, let go below it.
        assert!(trigger_is_down(TRIGGER_CLICKS - 0.01, true));
        assert!(trigger_is_down(TRIGGER_LETS_GO + 0.01, true));
        assert!(!trigger_is_down(TRIGGER_LETS_GO, true));
        assert!(!trigger_is_down(0.0, true));
    }

    /// The whole reason the clicks moved off the face buttons: `A` and `B`
    /// belong to the on-screen keyboard, which is drawn over the very
    /// application the pointer is aiming inside. Neither is anything to do
    /// with the pointer any more, and the triggers are nothing else.
    #[test]
    fn the_clicks_take_nothing_the_board_is_driven_with() {
        // `A` is Launch and `B` is Back on the shell's own screens, and the
        // board over an application is driven with exactly those two.
        assert_eq!(
            action_for_button(Button::South, 0, Layout::Mapped),
            Some(Action::Launch)
        );
        assert_eq!(
            action_for_button(Button::East, 0, Layout::Mapped),
            Some(Action::Back)
        );

        // The triggers are not actions anywhere: the shell has never had
        // anything bound to them, which is what leaves them free to be a
        // mouse even while the board is up. The bumpers beside them are the
        // ones that move between displays.
        for button in [Button::LeftTrigger2, Button::RightTrigger2] {
            assert_eq!(action_for_button(button, 0, Layout::Mapped), None);
            assert_eq!(chord_action(button, 0, Layout::Mapped, true), None);
        }
        assert_eq!(
            action_for_button(Button::LeftTrigger, 0, Layout::Mapped),
            Some(Action::PrevScreen)
        );

        // And on the pad read from its own report, the same: the stick presses
        // are the floating video's and the triggers are not buttons at all.
        assert_eq!(pad_action(Buttons::L3), None);
        assert_eq!(pad_action(Buttons::R3), Some(Action::Floating));
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
            triggers: (0.0, 0.0),
        };
        let frame = pad_frame(Buttons::DOWN, Buttons::DOWN, Buttons::empty());
        sticks.merge_pad(&frame);
        assert!(sticks.dpad[Direction::Down.index()]);
        assert!(!sticks.dpad[Direction::Up.index()]);
    }

    /// The Steam Controller's triggers reach the pointer by the same road
    /// every other pad's do: merged into one reading, furthest pull winning,
    /// and turned into clicks once.
    #[test]
    fn the_pads_triggers_merge_with_every_other_pads() {
        let mut sticks = Sticks {
            left: (0.0, 0.0),
            right: (0.0, 0.0),
            dpad: [false; Direction::COUNT],
            triggers: (0.0, 0.4),
        };
        let mut frame = pad_frame(Buttons::empty(), Buttons::empty(), Buttons::empty());
        frame.triggers = (0.9, 0.1);
        sticks.merge_pad(&frame);
        assert_eq!(sticks.triggers, (0.9, 0.4), "whichever is pulled further");

        let mut input = ControllerInput::new(false);
        assert_eq!(
            input.trigger_edges(sticks.triggers),
            vec![(BTN_RIGHT, true)],
            "the left trigger is the right button, and 0.4 is not a click"
        );
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

    /// Steam held with View asks for Valve's overlay, and spends the hold doing
    /// it — the shell's own guide must not also open behind it.
    #[test]
    fn the_pads_overlay_chord_spends_the_guide_buttons_hold() {
        let mut chorded = false;
        let steam_down = pad_frame(Buttons::STEAM, Buttons::STEAM, Buttons::empty());
        assert_eq!(pad_actions(&steam_down, &mut chorded), Vec::new());

        let with_view = pad_frame(
            Buttons::STEAM.union(Buttons::VIEW),
            Buttons::VIEW,
            Buttons::empty(),
        );
        assert_eq!(
            pad_actions(&with_view, &mut chorded),
            vec![Action::SteamOverlay]
        );
        assert!(chorded, "the hold has been spent");

        let steam_up = pad_frame(Buttons::VIEW, Buttons::empty(), Buttons::STEAM);
        assert_eq!(pad_actions(&steam_up, &mut chorded), Vec::new());
        assert!(!chorded);

        // View alone is still only the keyboard chord's modifier, whichever
        // way round the two are pressed.
        let view_only = pad_frame(Buttons::VIEW, Buttons::VIEW, Buttons::empty());
        assert_eq!(pad_actions(&view_only, &mut chorded), Vec::new());

        // And the keyboard is still reachable while both are held: `X` with
        // View is that chord wherever Steam happens to be.
        let with_x = pad_frame(
            Buttons::STEAM.union(Buttons::VIEW).union(Buttons::X),
            Buttons::X,
            Buttons::empty(),
        );
        assert_eq!(pad_actions(&with_x, &mut chorded), vec![Action::Keyboard]);
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

    /// `View` is only the chord's modifier, and `X` is not a table row at all:
    /// what it means depends on whether `View` is held with it, which is the
    /// one thing a table lookup cannot see. See [`the_pads_left_face_button_is_who_is_on_steam`].
    #[test]
    fn the_pads_chord_halves_do_nothing_apart() {
        assert_eq!(pad_action(Buttons::X), None);
        assert_eq!(pad_action(Buttons::VIEW), None);
        // Nor does the left stick press, which means nothing to this shell at
        // all. The right one is the videos floating over the guide, and is the
        // same button on this pad as on every other, which is the whole of why
        // it is asserted here too.
        assert_eq!(pad_action(Buttons::L3), None);
        assert_eq!(pad_action(Buttons::R3), Some(Action::Floating));
    }

    /// `X` alone raises the friends panel, on this pad as on every other.
    ///
    /// The regression this was written for: the left-hand face button reached
    /// nothing on a mapped pad and raised the *context menu* on an unmapped
    /// one, while the start screen's own corner had been naming it Friends the
    /// whole time — a legend for a button that does something else, which is
    /// the one thing a legend must never be. See `ui::start_hints`.
    #[test]
    fn the_pads_left_face_button_is_who_is_on_steam() {
        let mut chorded = false;
        let x = pad_frame(Buttons::X, Buttons::X, Buttons::empty());
        assert_eq!(pad_actions(&x, &mut chorded), vec![Action::Friends]);
        assert!(!chorded, "nothing was spelled on the guide button");

        // And with View held it is the board instead, which is the whole
        // reason this is not a `PAD_ACTIONS` row.
        let with_view = pad_frame(
            Buttons::VIEW.union(Buttons::X),
            Buttons::X,
            Buttons::empty(),
        );
        assert_eq!(
            pad_actions(&with_view, &mut chorded),
            vec![Action::Keyboard]
        );

        // The same press on the pads GilRs reads, under both of its namings
        // and under none at all. One button, one screen, whatever the pad.
        assert_eq!(
            action_for_button(Button::West, evdev::BTN_X, Layout::Mapped),
            Some(Action::Friends)
        );
        assert_eq!(
            action_for_button(Button::North, evdev::BTN_X, Layout::Guessed),
            Some(Action::Friends)
        );
        assert_eq!(
            action_for_button(Button::Unknown, evdev::BTN_THUMB2, Layout::Guessed),
            Some(Action::Friends)
        );
    }

    /// The pad this shell reads itself clicks with the same two controls the
    /// mapped pads do, so a hand moving between controllers finds the mouse
    /// buttons in one place — and it reports them as the edges a button has,
    /// out of a trigger that only ever says how far it is pulled.
    #[test]
    fn the_pads_triggers_are_the_same_two_clicks() {
        let mut input = ControllerInput::new(false);
        assert!(
            input.trigger_edges((0.0, 0.0)).is_empty(),
            "nothing at rest"
        );

        // The right trigger is the left button, as on a handheld.
        assert_eq!(input.trigger_edges((0.0, 1.0)), vec![(BTN_LEFT, true)]);
        // Held is not pressed again.
        assert!(input.trigger_edges((0.0, 1.0)).is_empty());
        // A finger easing off but not letting go holds the button down.
        assert!(input.trigger_edges((0.0, TRIGGER_CLICKS - 0.05)).is_empty());
        assert_eq!(input.trigger_edges((0.0, 0.0)), vec![(BTN_LEFT, false)]);

        // The left trigger is the right button, and the two are independent.
        assert_eq!(input.trigger_edges((1.0, 0.0)), vec![(BTN_RIGHT, true)]);
        assert_eq!(input.trigger_edges((1.0, 1.0)), vec![(BTN_LEFT, true)]);
        assert_eq!(
            input.trigger_edges((0.0, 0.0)),
            vec![(BTN_LEFT, false), (BTN_RIGHT, false)]
        );

        // And a trigger brushed on the way past is not a click at all.
        assert!(input.trigger_edges((0.3, 0.3)).is_empty());
    }

    /// The whole point of reading this pad from hidraw: the way out of a
    /// running application has to survive the gate that drops everything else,
    /// and so do the chords that photograph it and that reach Valve's overlay
    /// over it. None of the three is in the table — one is answered on a
    /// release and the others are chords — so what the table must not do is
    /// smuggle anything *else* past the gate.
    #[test]
    fn only_the_four_outside_actions_survive_a_running_application() {
        let mut actions: Vec<Action> = PAD_ACTIONS.iter().map(|(_, action)| *action).collect();
        actions.retain(survives_an_application);
        assert!(
            actions.is_empty(),
            "every plain press on this pad belongs to the application in front"
        );
        for action in [
            Action::Guide,
            Action::Keyboard,
            Action::Screenshot,
            Action::SteamOverlay,
        ] {
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

/// The triggers against a real kernel, on a pad the mapping database has never
/// heard of.
///
/// Everything above this is the rule; this is whether a controller keeps it.
/// The fact being pinned is one only a device can answer and one that is
/// invisible from the outside when it goes wrong: on a pad with no SDL entry
/// an analogue trigger is an **axis** and never a button, so a shell that read
/// the buttons alone had no mouse buttons at all there — the pointer moved,
/// aimed, and could not click. Measured on a `uinput` pad made for the purpose
/// on 2026-09-21, which is also where [`trigger_travel`]'s second half comes
/// from.
///
/// Skipped, rather than failed, where `/dev/uinput` cannot be written or where
/// the pad never reaches GilRs: a build machine without a seat is not a shell
/// with broken clicks.
#[cfg(test)]
mod hardware_tests {
    // `::evdev` throughout: this module has a private `evdev` of its own —
    // the raw button codes — and `use super::*` brings it into scope here.
    use super::*;
    use ::evdev::uinput::VirtualDevice;
    use ::evdev::{
        AbsInfo, AbsoluteAxisCode, AbsoluteAxisEvent, AttributeSet, BusType, InputId, KeyCode,
        UinputAbsSetup,
    };
    use std::io::ErrorKind;
    use std::path::PathBuf;
    use std::time::Instant;

    /// A pad nobody has heard of. Deliberately not a real vendor and product:
    /// a test that borrowed one would be testing SDL's database rather than
    /// this shell, and the case that matters is the pad the database misses.
    const TEST_NAME: &str = "LineXinBar Test Triggers";
    fn test_id() -> InputId {
        InputId::new(BusType::BUS_USB, 0x9a7e, 0x4d21, 0x0001)
    }

    const PATIENCE: Duration = Duration::from_secs(3);

    fn uinput_is_available() -> bool {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/uinput")
            .is_ok()
    }

    fn stick(axis: AbsoluteAxisCode) -> UinputAbsSetup {
        UinputAbsSetup::new(axis, AbsInfo::new(0, -32768, 32767, 16, 128, 0))
    }

    /// One trigger, over the byte `xpad` gives one.
    fn trigger(axis: AbsoluteAxisCode) -> UinputAbsSetup {
        UinputAbsSetup::new(axis, AbsInfo::new(0, 0, 255, 0, 0, 0))
    }

    /// A pad with two sticks, two triggers **as axes**, and the face buttons
    /// GilRs insists on before it will read anything at all.
    fn make_pad() -> std::io::Result<(VirtualDevice, PathBuf)> {
        let keys: AttributeSet<KeyCode> = [KeyCode::BTN_SOUTH, KeyCode::BTN_EAST]
            .into_iter()
            .collect();
        let mut pad = VirtualDevice::builder()?
            .name(TEST_NAME)
            .input_id(test_id())
            .with_keys(&keys)?
            .with_absolute_axis(&stick(AbsoluteAxisCode::ABS_X))?
            .with_absolute_axis(&stick(AbsoluteAxisCode::ABS_Y))?
            .with_absolute_axis(&stick(AbsoluteAxisCode::ABS_RX))?
            .with_absolute_axis(&stick(AbsoluteAxisCode::ABS_RY))?
            .with_absolute_axis(&trigger(AbsoluteAxisCode::ABS_Z))?
            .with_absolute_axis(&trigger(AbsoluteAxisCode::ABS_RZ))?
            .build()?;
        let node = pad
            .enumerate_dev_nodes_blocking()?
            .flatten()
            .find(|node| {
                node.file_name()
                    .is_some_and(|name| name.as_encoded_bytes().starts_with(b"event"))
            })
            .ok_or_else(|| {
                std::io::Error::new(ErrorKind::NotFound, "the test pad never got a device node")
            })?;
        Ok((pad, node))
    }

    /// Drain GilRs until its cached state is current, which is what every
    /// reading in this module is taken from.
    fn settle(gilrs: &mut Gilrs) {
        let deadline = Instant::now() + Duration::from_millis(300);
        while Instant::now() < deadline {
            while gilrs.next_event().is_some() {}
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn a_trigger_reported_as_an_axis_still_clicks() {
        if !uinput_is_available() {
            crate::skipped("/dev/uinput cannot be opened here");
            return;
        }
        let (mut pad, node) = make_pad().expect("a test pad can be made");
        // udev gives a joystick its node and then its permissions, and GilRs
        // can read neither until both have happened.
        let deadline = Instant::now() + PATIENCE;
        let mut gilrs = loop {
            if std::fs::File::open(&node).is_ok() {
                if let Ok(gilrs) = Gilrs::new() {
                    if gilrs
                        .gamepads()
                        .any(|(_, gamepad)| gamepad.name() == TEST_NAME)
                    {
                        break gilrs;
                    }
                }
            }
            if Instant::now() >= deadline {
                crate::skipped("the test pad never reached GilRs");
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        };

        let mut input = ControllerInput::new(false);
        let pull = |pad: &mut VirtualDevice, axis, value| {
            pad.emit(&[*AbsoluteAxisEvent::new(axis, value)])
                .expect("the test pad can be pulled");
        };

        // The right trigger, all the way down: the left mouse button, which is
        // what a handheld's `R2` does.
        pull(&mut pad, AbsoluteAxisCode::ABS_RZ, 255);
        settle(&mut gilrs);
        let sticks = Sticks::read(Some(&gilrs));
        assert!(
            sticks.triggers.1 > TRIGGER_CLICKS,
            "a trigger held down reads as pulled: {:?}",
            sticks.triggers
        );
        assert_eq!(input.trigger_edges(sticks.triggers), vec![(BTN_LEFT, true)]);

        // And let go.
        pull(&mut pad, AbsoluteAxisCode::ABS_RZ, 0);
        settle(&mut gilrs);
        let sticks = Sticks::read(Some(&gilrs));
        assert!(sticks.triggers.1 < TRIGGER_LETS_GO, "{:?}", sticks.triggers);
        assert_eq!(
            input.trigger_edges(sticks.triggers),
            vec![(BTN_LEFT, false)]
        );

        // The left trigger is the right button.
        pull(&mut pad, AbsoluteAxisCode::ABS_Z, 255);
        settle(&mut gilrs);
        let sticks = Sticks::read(Some(&gilrs));
        assert_eq!(
            input.trigger_edges(sticks.triggers),
            vec![(BTN_RIGHT, true)]
        );

        // Half way is not a click — the threshold is a threshold.
        pull(&mut pad, AbsoluteAxisCode::ABS_Z, 128);
        settle(&mut gilrs);
        let sticks = Sticks::read(Some(&gilrs));
        assert_eq!(
            input.trigger_edges(sticks.triggers),
            vec![(BTN_RIGHT, false)]
        );
    }
}
