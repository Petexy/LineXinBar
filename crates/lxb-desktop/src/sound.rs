//! The noise the shell itself makes.
//!
//! A console shell answers a direction twice: the selection moves, and it
//! clicks. The click is not decoration either — it is the half of the
//! acknowledgement that survives the user looking somewhere else on the
//! screen, and on a bar that eases rather than snaps it arrives on the press
//! while the highlight is still travelling. Every console shell of this shape
//! there has ever been made this sound.
//!
//! Eleven recordings, shipped in the repository beside this file for the same
//! reason the fonts and the cursor are: there may be no desktop on the machine
//! and so no theme of sounds to borrow one from. Ten are short answers.
//!
//! Six of those answer a control the user pressed, and they are arranged as
//! pairs. The start screen answers a direction that moved something and a
//! press that it then keeps; the guide overlay answers the same two in its own
//! voice, so the ear can tell the two screens apart as readily as the eye; the
//! move back out of a subcategory has its own, because it is the one move that
//! undoes one; a key of the on-screen keyboard going down has another, because
//! a board is its own instrument. A seventh belongs to the start screen alone:
//! an application starting from a tile of the bar, which is the one place in
//! the shell an application is started from. An eighth belongs to one *button*
//! rather than to a screen: the guide button, which works from inside anything
//! and therefore answers from inside anything.
//!
//! The last two answer something that *happened* rather than something that was
//! pressed, which is why each of them exists: a display photographed, and a
//! question from outside the session taking the screen to ask the user to prove
//! they may do something. Both are events the user did not ask for at that
//! moment, and a shell that made no noise for either would be one where a
//! picture and a password prompt both arrived in silence.
//!
//! The eleventh is the start screen's background music. It loops only while
//! nothing at all is open on any display, fades when an application takes one
//! of them, and is reconstructed from sample zero whenever the start screen is
//! returned to rather than resumed.
//!
//! It is also the one recording here that can be turned off outright, from
//! Settings > Sounds > Start music. The ten short clips answer a control that
//! was pressed or an event that arrived, and a shell that answered a press with
//! nothing would be a shell with a dead button on it; a background is the one
//! sound the shell makes at somebody who has pressed nothing, which is exactly
//! what a person reading in the same room might not want. Turned off it stops
//! the way muting stops it — at once, and not with the fade an application gets
//! — and turned back on it begins again from the beginning.
//!
//! Reusable panels raised from either screen — context menus, dialogs, and the
//! mixer they can carry — retain the same sounds they have everywhere else: a
//! component decides its own voice rather than inheriting the backdrop it
//! happened to be raised over. A guide raised from an otherwise empty start
//! screen does not end its background music; one raised over an application
//! does not start it.
//!
//! Their level comes from the System row of the guide's volume mixer, and is
//! kept in [`crate::settings`] with everything else the shell remembers about
//! itself. That row is the shell's own sounds and nothing else's: what the
//! whole session comes out at is the volume bar in the sidebar above it, which
//! is there whether or not the mixer opens.
//!
//! Playing is in-process, unlike everything else this shell does with audio.
//! The volume bars in the guide shell out to `wpctl` and friends — see
//! [`crate::system`] — because setting the session's volume is a thing the
//! session manager owns and answers for. An interface sound is the opposite:
//! it is one more stream playing *into* that session, it has to start within a
//! frame of the button, and a process spawned per step would be a fork every
//! 90 ms under a held D-pad. The ten short effects are decoded once; the much
//! longer music is streamed from its embedded Vorbis bytes and rewound by a
//! fresh decoder, so it does not cost a decoded track's worth of memory.

use std::io::Cursor;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rodio::buffer::SamplesBuffer;
use rodio::cpal::{self, traits::HostTrait, StreamError};
use rodio::{
    Decoder, DeviceSinkBuilder, DeviceSinkError, DeviceTrait, MixerDeviceSink, Player, Sample,
    Source,
};

use crate::settings;
use crate::system::Level;

/// One of the short recordings, and the one place their order is decided: the
/// samples and the last time each was started are held in arrays under these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Effect {
    Step,
    Back,
    Select,
    GuideStep,
    GuideSelect,
    Key,
    Launch,
    Shutter,
    Authenticate,
    GuideOpen,
    Notify,
}

impl Effect {
    const ALL: [Effect; 11] = [
        Effect::Step,
        Effect::Back,
        Effect::Select,
        Effect::GuideStep,
        Effect::GuideSelect,
        Effect::Key,
        Effect::Launch,
        Effect::Shutter,
        Effect::Authenticate,
        Effect::GuideOpen,
        Effect::Notify,
    ];

    /// The recording it plays.
    fn recording(self) -> &'static [u8] {
        match self {
            Effect::Step => STEP,
            Effect::Back => BACK,
            Effect::Select => SELECT,
            Effect::GuideStep => GUIDE_STEP,
            Effect::GuideSelect => GUIDE_SELECT,
            Effect::Key => KEY,
            Effect::Launch => LAUNCH,
            Effect::Shutter => SHUTTER,
            Effect::Authenticate => AUTHENTICATE,
            Effect::GuideOpen => GUIDE_OPEN,
            Effect::Notify => NOTIFY,
        }
    }

    /// What that recording is called, for a warning somebody has to act on.
    fn name(self) -> &'static str {
        match self {
            Effect::Step => "press.ogg",
            Effect::Back => "press-back.ogg",
            Effect::Select => "press-selected.ogg",
            Effect::GuideStep => "press-guide.ogg",
            Effect::GuideSelect => "press-guide-selected.ogg",
            Effect::Key => "keyboard-click.ogg",
            Effect::Launch => "app-launch.ogg",
            Effect::Shutter => "screenshot.ogg",
            Effect::Authenticate => "polkit.ogg",
            Effect::GuideOpen => "guide-open.ogg",
            Effect::Notify => "notification.ogg",
        }
    }
}

/// The click one step of the selection makes.
const STEP: &[u8] = include_bytes!("sounds/press.ogg");

/// The step back out of a subcategory.
///
/// A pair with [`STEP`] rather than a sound of its own kind: going back is a
/// move like any other, and what makes it worth a second clip is that it is
/// the one move that undoes one. The bar is a path walked into, and a shell
/// that answered walking in and walking out with the same noise would leave
/// the ear no way of telling which way the user is going.
const BACK: &[u8] = include_bytes!("sounds/press-back.ogg");

/// Choosing something on the start screen that the shell then keeps: a
/// subcategory opened, a setting taken, a search field raised.
///
/// The counterpart of [`STEP`] rather than a louder version of it. Moving the
/// highlight and pressing what it is on are the two halves of using the bar,
/// and until now only the first half had an answer.
const SELECT: &[u8] = include_bytes!("sounds/press-selected.ogg");

/// A direction that moved something in the Home Button guide.
///
/// The guide's own, not the bar's. It is a screen of its own rather than
/// another column of the start screen, and the two are told apart by ear as
/// well as by eye — which is the whole reason it waited rather than borrowing
/// [`STEP`].
const GUIDE_STEP: &[u8] = include_bytes!("sounds/press-guide.ogg");

/// Choosing something in the guide that the shell then keeps.
///
/// [`SELECT`] is to [`STEP`] as this is to [`GUIDE_STEP`]: each screen answers
/// a move and a press in its own voice.
const GUIDE_SELECT: &[u8] = include_bytes!("sounds/press-guide-selected.ogg");

/// A key of the on-screen keyboard going down.
///
/// Its own sound rather than the bar's, because the board is its own
/// instrument: walking across its keys is walking a bar and clicks like one,
/// and putting a key *down* is the thing that only happens there.
const KEY: &[u8] = include_bytes!("sounds/keyboard-click.ogg");

/// An application starting.
const LAUNCH: &[u8] = include_bytes!("sounds/app-launch.ogg");

/// A whole display photographed.
///
/// The one sound here that answers something the shell did rather than
/// something the user pressed, and it is the pair of the white flash the
/// compositor draws over that display — the two halves of one acknowledgement,
/// for the two senses. It sounds only once the file is on the disk, so it says
/// a picture exists rather than that a chord was spelled, and it does not sound
/// at all for the picture of a single window: that one is answered by a panel
/// naming the folder, which is a screen the user is looking at anyway.
const SHUTTER: &[u8] = include_bytes!("sounds/screenshot.ogg");

/// A question from outside the session, arriving on screen: the machine asking
/// the user to prove they may do something.
///
/// The other clip that answers something the shell did rather than something
/// the user pressed, and it is here for a reason [`SHUTTER`]'s is not. That one
/// accompanies a thing the user asked for a moment earlier; this one announces
/// a panel **nobody asked for**, raised over whatever was in front of them
/// because a program somewhere wants a password. It is the half of the arrival
/// that reaches somebody looking at the other screen, or at the room.
///
/// Once, when the question goes up — not again when a refused password is asked
/// for a second time. The panel is already in front of the user by then, and a
/// sound that said "here is a new question" would be untrue: it is the same
/// one.
const AUTHENTICATE: &[u8] = include_bytes!("sounds/polkit.ogg");

/// The Home Button guide arriving, because the user asked for it.
///
/// The overlay used to come up in silence, and the silence was deliberate:
/// opening a screen is not choosing anything on it, and a shell that announced
/// every screen it drew would be a noisy one. What changes that is *which
/// control* this is. The guide button is the one control in the shell that
/// works from inside anything — a game holding every other key, a panel, the
/// board — and the press that summons the overlay is therefore the one press
/// whose answer cannot be "look at the screen and see": the user may have
/// pressed it precisely because what is on the screen has stopped listening.
///
/// So it belongs to the button and not to the overlay. The guide opened by any
/// other route stays silent — Back walking out of the top of the bar, a
/// question from a portal, an authorisation panel raised over a game — because
/// none of those is somebody asking for the guide, and a sound saying "here it
/// is" would be answering a press that nobody made. Closing it is silent too:
/// the overlay leaving is the screen behind it coming back, which is its own
/// answer.
const GUIDE_OPEN: &[u8] = include_bytes!("sounds/guide-open.ogg");

/// Something announced to the session, arriving in the corner of the screen.
///
/// The third clip that answers something which *happened* rather than something
/// the user pressed, and it belongs to that group for the same reason
/// [`AUTHENTICATE`] does: a bubble is raised over whatever was in front of
/// somebody because a program somewhere had news. It is the half of the arrival
/// that reaches a person looking at the other screen, or at the room, or at the
/// game they are playing rather than at its top-right corner.
///
/// Only when a bubble is raised, and not when an announcement is merely filed.
/// A program that marked something *low* has said it is not worth interrupting
/// anyone over — see [`crate::notify::Urgency`] — and a shell that made a noise
/// for it anyway would be overruling the one hint this daemon takes at its word.
/// Nor once per bubble in a burst that arrives together: see
/// [`Sounds::notified`].
///
/// **This recording is a placeholder.** It is two struck bells a fifth apart,
/// rising, synthesised rather than recorded — the shape is right and the
/// character is not, and it is here so that the path is wired and audible while
/// a real one is found. Everything about it is disposable except its shape:
/// about a second, decaying to true silence so it never cuts off, and quiet
/// enough to sit under whatever is already playing.
const NOTIFY: &[u8] = include_bytes!("sounds/notification.ogg");

/// The start screen's background music.
const MUSIC: &[u8] = include_bytes!("sounds/start-bg-music.ogg");

/// The music eases in from silence so beginning at sample zero never clicks.
const MUSIC_FADE_IN: Duration = Duration::from_millis(400);

/// How long the Start music remains while an application takes the display.
const MUSIC_FADE_OUT: Duration = Duration::from_millis(600);

/// The shortest gap between two copies of one clip.
///
/// Copies of one recording laid over each other add, and being identical they
/// add in phase: ten of the same click started in the same instant is that
/// click twenty decibels louder. A wheel asks for exactly that. One
/// `wl_pointer.axis` event can be worth several rows — a flick of a
/// high-resolution wheel reports them in hundred-and-twentieths of a notch, all
/// of it arriving at once — and every row it crosses is a move that clicks, so
/// a fast scroll used to answer with one very loud one.
///
/// Below this the request is dropped rather than mixed, which is the honest
/// answer: two clicks a sixteenth of a second apart are not two things anyone
/// can hear separately, so nothing is lost and the shell cannot be made loud by
/// being driven quickly.
///
/// Longer than every one of the clicks, which are the sounds a fast input can
/// ask for over and over, and shorter than
/// [`crate::controller::REPEAT_INTERVAL`], so
/// a held direction still walks the bar to a run of them. The two long
/// recordings — the step back out of a subcategory, and an application taking
/// the display — can still lie over themselves, and should: those are two moves
/// the user really did make rather than one gesture multiplied.
const RESTED: Duration = Duration::from_millis(60);

/// How long to leave the output alone after failing to open it.
///
/// The session's sound server is very often not up yet when the shell is: both
/// are started at login, and this one draws its first frame in well under a
/// second. So a device that is not there at start-up is a device that is
/// probably coming, and the shell tries again the next time it has something
/// to play — but no faster than this, because the other reason a device fails
/// to open is that the machine has no sound card at all, and that answer must
/// not be paid for on every press of the D-pad.
const RETRY_AFTER: Duration = Duration::from_secs(5);

struct MusicPlayback {
    player: Player,
    started_at: Instant,
    fade_out: Option<MusicFade>,
}

#[derive(Clone, Copy)]
struct MusicFade {
    started_at: Instant,
    from_volume: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MusicTransition {
    Keep,
    StartFromBeginning,
    FadeOut,
}

/// The shell's sounds, and the output they go to.
///
/// Silence is a working state, not a failure: a machine with no sound card, a
/// session whose server never starts, a clip that will not decode. The shell
/// is driven by a controller and drawn on a screen, and none of that stops
/// because nothing can be heard.
pub struct Sounds {
    /// The open output device. Nothing ever reads it — it is held because
    /// dropping it closes the device and silences everything playing through
    /// it.
    device: Option<MixerDeviceSink>,
    /// Set by the output thread when its device disappears or its stream is
    /// invalidated. The next sound drops that dead stream and opens the
    /// machine's current default instead.
    device_failed: Arc<AtomicBool>,
    /// The clips, decoded once at start-up rather than on each press, in the
    /// order [`Effect`] lists them. Between them they are under two seconds of
    /// audio: the samples cost less than a handful of icons, and decoding
    /// Vorbis inside the input handler would put a codec on the path between a
    /// button and the frame that answers it.
    effects: [Option<SamplesBuffer>; Effect::ALL.len()],
    /// When each of them was last put on the output, so that a copy is never
    /// laid on top of one still sounding. See [`RESTED`].
    played_at: [Option<Instant>; Effect::ALL.len()],
    /// The one looping stream, while the start screen owns the focused
    /// display. Unlike the effects it is decoded as it plays, because it is
    /// minutes rather than milliseconds long.
    music: Option<MusicPlayback>,
    /// The last focus decision. A rising edge is what makes a *new* decoder;
    /// it must not be inferred from whether a fade still has a player alive.
    music_wanted: bool,
    /// A bundled decoder failure cannot recover on the next frame, and must
    /// not turn the main loop into a stream of identical warnings.
    music_broken: bool,
    /// The earliest another attempt at opening the device may be made. See
    /// [`RETRY_AFTER`].
    retry_at: Instant,
}

impl Sounds {
    pub fn new() -> Self {
        let mut sounds = Self {
            device: None,
            device_failed: Arc::new(AtomicBool::new(false)),
            effects: Effect::ALL.map(|effect| decode(effect.name(), effect.recording())),
            played_at: [None; Effect::ALL.len()],
            music: None,
            music_wanted: false,
            music_broken: false,
            retry_at: Instant::now(),
        };
        // Eagerly, so that the first click of the session is as prompt as
        // every one after it: opening a device takes long enough to hear.
        sounds.open();
        sounds
    }

    /// Click, for a direction that moved something.
    pub fn step(&mut self) {
        self.play(Effect::Step);
    }

    /// The other click, for a step back out of a subcategory.
    ///
    /// Whichever control made it. Back is the button for it and Left is the
    /// direction, and they are the same move — a shell where the two of them
    /// left a column with different sounds would be a shell with two ways out
    /// of it.
    pub fn back(&mut self) {
        self.play(Effect::Back);
    }

    /// The start screen's answer to a press that the shell then keeps: a
    /// subcategory opened, a setting taken, a search field raised.
    ///
    /// Not a press on a tile that hands the display to an application. That is
    /// [`Self::launch`] whether the process is being started or merely returned
    /// to, because a tile of the bar is where an application is started from
    /// and the user pressing it has done the same thing either way.
    pub fn select(&mut self) {
        self.play(Effect::Select);
    }

    /// The guide's own click, for a direction that moved something in it.
    pub fn guide_step(&mut self) {
        self.play(Effect::GuideStep);
    }

    /// The guide's answer to every press it acts on — a panel raised out of a
    /// tile, a switch turned over, the overlay dismissed back to the start
    /// screen, and the presses that hand an application the display: a window
    /// card chosen out of the deck, and Resume with something running behind
    /// the overlay.
    ///
    /// Those two used to take [`Self::launch`], on the grounds that being
    /// handed the screen is one event however it was asked for. They do not
    /// now: that clip belongs to the screen an application is *started* from,
    /// and a screen with a voice of its own must not borrow another's for half
    /// of its rows. The guide only ever returns to something already up.
    pub fn guide_select(&mut self) {
        self.play(Effect::GuideSelect);
    }

    /// The on-screen keyboard's own click, for a key of it going down.
    ///
    /// Every key of it, including the ones that type nothing: Shift and the
    /// key that puts the board away are keys the user pressed, and a board
    /// where two of the keys answered silently would be a board with two dead
    /// keys on it.
    pub fn key(&mut self) {
        self.play(Effect::Key);
    }

    /// An application about to take the display, started from the start
    /// screen.
    ///
    /// A tile of the bar pressed, whether that forks a process or comes back
    /// to one already up: the tile is where an application is started from,
    /// and the user pressing it has done the same thing either way. The guide
    /// is not one of these — see [`Self::guide_select`], which answers its
    /// window cards and its Resume row. A press that failed to start anything
    /// is not one either.
    pub fn launch(&mut self) {
        self.play(Effect::Launch);
    }

    /// A whole display photographed, and the picture already written.
    ///
    /// Said at the same moment the display flashes, because they are one
    /// answer given twice: a person looking at the screen sees it, and a
    /// person who spelled the chord without looking hears it. Nothing else
    /// about the shell changes — no panel, no row — so if neither reached the
    /// user there would be nothing at all to say a picture had been taken.
    ///
    /// Only for the picture of a whole display. See [`SHUTTER`].
    pub fn shutter(&mut self) {
        self.play(Effect::Shutter);
    }

    /// The machine asking the user to prove they may do something — see
    /// [`crate::polkit`] and [`AUTHENTICATE`].
    ///
    /// Said as the panel goes up, and only if it went up: a question the shell
    /// could not put on screen is one the user cannot answer, and a noise for
    /// it would send them looking for something that is not there.
    pub fn authenticate(&mut self) {
        self.play(Effect::Authenticate);
    }

    /// The guide button pressed, and the overlay coming up because of it.
    ///
    /// Only that: the press has to have *opened* the guide, and it has to have
    /// been the guide button that did it. See [`GUIDE_OPEN`] for both halves of
    /// why, and note that the shell opens the overlay itself for two questions
    /// that arrive from outside it — neither of which sounds this.
    pub fn guide_open(&mut self) {
        self.play(Effect::GuideOpen);
    }

    /// Something announced to the session, and a bubble raised for it — see
    /// [`NOTIFY`] and [`crate::notify::Center`].
    ///
    /// Once for the frame, however many arrived on it. Three programs that all
    /// have something to say at the moment a session comes back from suspend
    /// are three bubbles and one sound: the noise says *there is something in
    /// the corner*, which is as true of three as of one, and saying it three
    /// times over would be the shell shouting about the very thing it has just
    /// decided is not worth interrupting anyone for.
    pub fn notified(&mut self) {
        self.play(Effect::Notify);
    }

    /// Reconcile the start screen's one looping stream with what has focus.
    ///
    /// Called on every main-loop tick rather than from individual focus-event
    /// handlers. Both local guide actions and compositor foreground events can
    /// change the answer, and by this point all of them have settled. Repeated
    /// calls with the same answer only move an existing envelope; a false to
    /// true edge always destroys any tail still fading and starts a fresh
    /// decoder at sample zero.
    pub fn sync_music(&mut self, wanted: bool, now: Instant) {
        self.refresh_failed_output(now);

        let transition = music_transition(self.music_wanted, wanted);
        self.music_wanted = wanted;
        match transition {
            MusicTransition::Keep => {}
            MusicTransition::StartFromBeginning => self.stop_music(),
            MusicTransition::FadeOut => {
                if let Some(music) = self.music.as_mut() {
                    music.fade_out = Some(MusicFade {
                        started_at: now,
                        from_volume: music.player.volume(),
                    });
                    tracing::debug!(
                        milliseconds = MUSIC_FADE_OUT.as_millis() as u64,
                        "Start music is fading for application focus"
                    );
                }
            }
        }

        let level = settings::sound();
        if !music_allowed(level, settings::start_music()) {
            // Both of these are immediate instructions rather than application
            // handoffs, so neither gets the fade one does. Dropping the stream
            // is also what makes coming back begin at zero instead of revealing
            // a track that advanced where nobody could hear it.
            self.stop_music();
            return;
        }

        if wanted {
            if self.music.is_none() {
                self.start_music(now);
            }
            if let Some(music) = self.music.as_mut() {
                let envelope = fade_in_gain(now.saturating_duration_since(music.started_at));
                music
                    .player
                    .set_volume(normalized_volume(level.value) * envelope);
            }
            return;
        }

        let mut finished = false;
        if let Some(music) = self.music.as_mut() {
            // The usual route installs this on the true-to-false edge. Keep
            // the invariant defensive too: a player must never remain at full
            // volume merely because it survived an output callback in an
            // unexpected order.
            let fade = *music.fade_out.get_or_insert(MusicFade {
                started_at: now,
                from_volume: music.player.volume(),
            });
            let volume = fade_out_volume(
                fade.from_volume,
                now.saturating_duration_since(fade.started_at),
            );
            music.player.set_volume(volume);
            finished = volume <= 0.0;
        }
        if finished {
            self.stop_music();
            tracing::debug!("Start music stopped after its fade");
        }
    }

    /// Put one clip on the machine's output.
    ///
    /// Every call is its own sound rather than one restarted, so a D-pad held
    /// down walks the bar to a run of clicks instead of to one click held at
    /// the point the repeats overtook it — and so a launch pressed while the
    /// last one is still playing is heard rather than swallowed. What it is
    /// not is a clip laid over a copy of itself: see [`RESTED`], which is the
    /// one gap too short to be two sounds and is what keeps a spun wheel from
    /// answering in one very loud click.
    ///
    /// At whatever the mixer's System row is set to — read here rather than
    /// held, so that the row moved by a direction is heard at its new level by
    /// the very click that direction makes.
    fn play(&mut self, effect: Effect) {
        let level = settings::sound();
        if level.muted || level.value <= 0.0 {
            return;
        }
        let now = Instant::now();
        if !rested(self.played_at[effect as usize], now) {
            return;
        }
        // Cloning is cheap: the samples are shared, and what is copied is a
        // cursor over them.
        let Some(clip) = self.effects[effect as usize].clone() else {
            return;
        };
        // CPAL reports a device disappearing on the audio thread. It cannot
        // safely rebuild the shell's output there, so the callback leaves one
        // bit for this thread to consume on the next press. A fresh flag is
        // installed with every stream, which keeps a late callback from an
        // old device from tearing down its replacement.
        self.refresh_failed_output(now);
        if self.device.is_none() {
            self.open();
        }
        if let Some(device) = &self.device {
            // Normalised rather than plain amplification: a slider is read as
            // loudness and amplitude is not loudness, so a row dragged to the
            // middle should sound half as loud rather than measure half as
            // tall. This is the curve every volume control has.
            device.mixer().add(clip.amplify_normalized(level.value));
            // Noted only once it is really on the output. A press that found no
            // device made no sound, and must not stand in the way of the next
            // one that finds a device to make it on.
            self.played_at[effect as usize] = Some(now);
        }
    }

    /// Start a separately controlled, streaming music source at sample zero.
    fn start_music(&mut self, now: Instant) {
        if self.music_broken {
            return;
        }
        if self.device.is_none() {
            self.open();
        }
        let Some(device) = self.device.as_ref() else {
            return;
        };
        let source = match Decoder::new_looped(Cursor::new(MUSIC)) {
            Ok(source) => source,
            Err(err) => {
                tracing::warn!(%err, "bundled Start music will not decode");
                self.music_broken = true;
                return;
            }
        };
        let player = Player::connect_new(device.mixer());
        player.set_volume(0.0);
        player.append(source);
        self.music = Some(MusicPlayback {
            player,
            started_at: now,
            fade_out: None,
        });
        tracing::debug!("Start music began from the beginning");
    }

    fn stop_music(&mut self) {
        if let Some(music) = self.music.take() {
            music.player.stop();
        }
    }

    /// Consume the audio thread's signal before using its mixer again.
    fn refresh_failed_output(&mut self, now: Instant) {
        if self.device_failed.swap(false, Ordering::AcqRel) {
            tracing::warn!("audio output was lost; reopening it");
            // A Player is tied to its mixer. Drop it before the mixer, and
            // if the start screen still owns focus the next lines of
            // `sync_music` will build a new decoder on the replacement output
            // at sample zero.
            self.stop_music();
            self.device = None;
            self.retry_at = now;
        }
    }

    /// Open the machine's output, unless an attempt failed too recently.
    fn open(&mut self) {
        if Instant::now() < self.retry_at {
            return;
        }
        let device_failed = Arc::new(AtomicBool::new(false));
        match open_output(Arc::clone(&device_failed)) {
            Ok(device) => {
                tracing::info!("shell audio ready");
                self.device_failed = device_failed;
                self.device = Some(device);
            }
            Err(err) => {
                // Logged at every attempt rather than only the first: the
                // attempts are minutes apart, and what stopped the sound is
                // the one thing a user with a silent shell will go looking
                // for.
                tracing::warn!(%err, "no audio output; the shell will be silent");
                self.retry_at = Instant::now() + RETRY_AFTER;
            }
        }
    }
}

/// Whether enough has passed since one clip last played for it to play again.
///
/// A clip that has never played has, so the first of anything is always heard.
fn rested(played_at: Option<Instant>, now: Instant) -> bool {
    played_at.is_none_or(|last| now.saturating_duration_since(last) >= RESTED)
}

/// Whether the music may be on the output at all, before anything about focus
/// is asked.
///
/// Two settings, and they are not the same statement. The level is how loud
/// *everything* the shell plays is, the clicks included, and a muted shell is a
/// shell whose music is one of the things that has been silenced. Settings >
/// Sounds > Start music is about the music alone: a session that answers every
/// press as loudly as ever and plays nothing at the user who has pressed
/// nothing.
///
/// Either of them is enough on its own, and neither undoes the other: music
/// turned off is not turned back on by unmuting, and a shell muted with the
/// music on comes back playing it.
fn music_allowed(level: Level, playing: bool) -> bool {
    playing && !level.muted && level.value > 0.0
}

fn music_transition(previous: bool, wanted: bool) -> MusicTransition {
    match (previous, wanted) {
        (false, true) => MusicTransition::StartFromBeginning,
        (true, false) => MusicTransition::FadeOut,
        _ => MusicTransition::Keep,
    }
}

/// The same perceptual curve [`Source::amplify_normalized`] applies to the
/// effects, exposed as a number because a [`Player`]'s volume changes while it
/// is already playing.
fn normalized_volume(value: f32) -> f32 {
    let value = value.clamp(0.0, 1.0);
    let mut amplitude = f32::exp(6.907_755_4 * value) / 1000.0;
    if value < 0.1 {
        amplitude *= value * 10.0;
    }
    amplitude
}

fn smooth_step(elapsed: Duration, duration: Duration) -> f32 {
    let progress = (elapsed.as_secs_f32() / duration.as_secs_f32()).clamp(0.0, 1.0);
    progress * progress * (3.0 - 2.0 * progress)
}

fn fade_in_gain(elapsed: Duration) -> f32 {
    smooth_step(elapsed, MUSIC_FADE_IN)
}

fn fade_out_volume(from: f32, elapsed: Duration) -> f32 {
    from * (1.0 - smooth_step(elapsed, MUSIC_FADE_OUT))
}

/// Open the default output with a callback that can tell [`Sounds`] it has
/// ceased to be usable.
///
/// This keeps rodio's default-device fallback: if the preferred configuration
/// or device cannot be opened, every real output is tried before the shell
/// accepts silence. `open_default_sink` performs the same walk, but fixes its
/// own callback and therefore cannot report a device that disappears later.
fn open_output(device_failed: Arc<AtomicBool>) -> Result<MixerDeviceSink, DeviceSinkError> {
    let callback = output_error_callback(device_failed);
    DeviceSinkBuilder::from_default_device()
        .and_then(|builder| builder.with_error_callback(callback.clone()).open_stream())
        .or_else(|original_err| {
            let devices = match cpal::default_host().output_devices() {
                Ok(devices) => devices,
                Err(err) => {
                    tracing::error!(%err, "could not list fallback audio outputs");
                    return Err(original_err);
                }
            };
            devices
                .filter(|device| {
                    device
                        .description()
                        .map(|description| {
                            description.driver().is_some_and(|driver| driver != "null")
                        })
                        .unwrap_or(false)
                })
                .find_map(|device| {
                    DeviceSinkBuilder::from_device(device)
                        .and_then(|builder| {
                            builder
                                .with_error_callback(callback.clone())
                                .open_sink_or_fallback()
                        })
                        .ok()
                })
                .ok_or(original_err)
        })
}

/// The callback carried by one device stream.
///
/// Underruns are glitches the stream itself can survive. A vanished device or
/// invalid configuration cannot recover in place; those are the two errors
/// rodio documents as requiring the stream to be destroyed and rebuilt.
fn output_error_callback(
    device_failed: Arc<AtomicBool>,
) -> impl FnMut(StreamError) + Clone + Send + 'static {
    move |err| {
        let needs_reopen = matches!(
            err,
            StreamError::DeviceNotAvailable | StreamError::StreamInvalidated
        );
        if needs_reopen {
            device_failed.store(true, Ordering::Release);
        }
        tracing::error!(%err, needs_reopen, "audio output stream error");
    }
}

/// Decode one of the shipped clips into samples ready to play.
///
/// A failure here is a fault in the build rather than in the session — the
/// clip is compiled into the binary — so it is reported and then left alone:
/// there is no later attempt that could go differently.
fn decode(name: &str, clip: &'static [u8]) -> Option<SamplesBuffer> {
    let decoder = match Decoder::new(Cursor::new(clip)) {
        Ok(decoder) => decoder,
        Err(err) => {
            tracing::warn!(sound = name, %err, "bundled sound will not decode");
            return None;
        }
    };
    let channels = decoder.channels();
    let sample_rate = decoder.sample_rate();
    let samples: Vec<Sample> = decoder.collect();
    if samples.is_empty() {
        tracing::warn!(sound = name, "bundled sound decoded to nothing");
        return None;
    }
    Some(SamplesBuffer::new(channels, sample_rate, samples))
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::controller;

    /// Every clip the shell ships is a sound, whatever the machine running the
    /// tests can play. Nothing here opens a device: this asserts about the
    /// bytes in the binary, which is the half of it that a build can break.
    #[test]
    fn the_bundled_clips_decode() {
        for effect in Effect::ALL {
            let name = effect.name();
            let clip = decode(name, effect.recording()).unwrap_or_else(|| panic!("{name} decodes"));
            assert!(
                clip.total_duration()
                    .is_some_and(|held| held.as_millis() > 0),
                "{name} has no samples in it, so it is a silent one"
            );
        }

        // The music deliberately is not passed through `decode`: collecting
        // a two-and-a-half-minute stereo track into memory is exactly what the
        // runtime avoids. Opening it and reaching a sample proves the embedded
        // stream is one the same Vorbis decoder can play.
        let music = Decoder::new(Cursor::new(MUSIC)).expect("start-bg-music.ogg decodes");
        let one_minute = music.sample_rate().get() as usize * music.channels().get() as usize * 60;
        assert_eq!(
            music.take(one_minute).count(),
            one_minute,
            "Start music is not the expected background-length recording"
        );
    }

    /// The rule that keeps a spun wheel from answering in one very loud click:
    /// a scroll event worth several rows asks for several of the same clip in
    /// the same instant, and identical samples laid over each other add.
    #[test]
    fn a_clip_is_never_laid_on_top_of_a_copy_of_itself() {
        let start = Instant::now();
        assert!(rested(None, start), "the first of anything is heard");
        assert!(!rested(Some(start), start), "the same instant is not twice");
        assert!(!rested(
            Some(start),
            start + RESTED - Duration::from_millis(1)
        ));
        assert!(rested(Some(start), start + RESTED));

        // Which holds for every click, because none of them lasts that long.
        // The two longer recordings are left out: neither the step back out of
        // a subcategory nor an application taking the display can be asked for
        // at this rate without the shell having done something else between.
        for effect in [
            Effect::Step,
            Effect::Select,
            Effect::GuideStep,
            Effect::GuideSelect,
            Effect::Key,
        ] {
            let clip = decode(effect.name(), effect.recording()).expect("a click decodes");
            let held = clip.total_duration().expect("a click has a length");
            assert!(
                held <= RESTED,
                "{} is {held:?}, so two of them {RESTED:?} apart would overlap",
                effect.name()
            );
        }

        // And a held direction is still a run of clicks rather than one click:
        // the repeats are further apart than this.
        assert!(RESTED < controller::REPEAT_INTERVAL);
    }

    #[test]
    fn each_music_decoder_begins_at_the_same_sample() {
        let intro = || {
            Decoder::new_looped(Cursor::new(MUSIC))
                .expect("Start music loops")
                .take(1_024)
                .collect::<Vec<_>>()
        };
        assert_eq!(intro(), intro());
    }

    /// The two settings that silence the music, and what each of them is
    /// saying. Focus is a separate question and is asked after these.
    #[test]
    fn either_setting_silences_the_music_and_neither_undoes_the_other() {
        let loud = Level {
            value: 1.0,
            muted: false,
        };
        assert!(music_allowed(loud, true));

        // Settings > Sounds > Start music, off. The clicks are as loud as ever.
        assert!(!music_allowed(loud, false));

        // The mixer's System row, silenced or dragged to the bottom. That one
        // takes the whole shell with it, so the music goes whether or not it is
        // the thing being turned off.
        let muted = Level {
            value: 1.0,
            muted: true,
        };
        let down = Level {
            value: 0.0,
            muted: false,
        };
        for level in [muted, down] {
            assert!(!music_allowed(level, true), "the shell itself is silent");
            assert!(!music_allowed(level, false));
        }
    }

    #[test]
    fn music_focus_edges_restart_or_fade_only_once() {
        assert_eq!(
            music_transition(false, true),
            MusicTransition::StartFromBeginning
        );
        assert_eq!(music_transition(true, true), MusicTransition::Keep);
        assert_eq!(music_transition(true, false), MusicTransition::FadeOut);
        assert_eq!(music_transition(false, false), MusicTransition::Keep);

        // While the old Player is still fading, intent is already false. A
        // return therefore takes the fresh-decoder path rather than reversing
        // that Player and continuing from the middle.
        assert_eq!(
            music_transition(false, true),
            MusicTransition::StartFromBeginning
        );
    }

    #[test]
    fn the_music_envelopes_reach_their_exact_ends() {
        assert_eq!(fade_in_gain(Duration::ZERO), 0.0);
        assert_eq!(fade_in_gain(MUSIC_FADE_IN), 1.0);
        assert_eq!(fade_out_volume(0.37, Duration::ZERO), 0.37);
        assert_eq!(fade_out_volume(0.37, MUSIC_FADE_OUT), 0.0);
        assert!(
            fade_out_volume(0.37, MUSIC_FADE_OUT / 2) < fade_out_volume(0.37, MUSIC_FADE_OUT / 4)
        );
    }

    #[test]
    fn music_uses_the_effects_perceptual_volume_curve() {
        assert_eq!(normalized_volume(0.0), 0.0);
        assert!((normalized_volume(1.0) - 1.0).abs() < f32::EPSILON * 2.0);
        assert!((normalized_volume(0.5) - 0.031_622_775).abs() < 0.000_001);
    }

    #[test]
    fn a_lost_or_invalidated_output_is_reopened_but_an_underrun_is_not() {
        let failed = Arc::new(AtomicBool::new(false));
        let mut callback = output_error_callback(Arc::clone(&failed));

        callback(StreamError::BufferUnderrun);
        assert!(!failed.load(Ordering::Acquire));

        callback(StreamError::DeviceNotAvailable);
        assert!(failed.swap(false, Ordering::AcqRel));

        callback(StreamError::StreamInvalidated);
        assert!(failed.load(Ordering::Acquire));
    }
}
