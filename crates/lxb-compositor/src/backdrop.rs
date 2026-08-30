//! The picture on screen whenever the session itself has nothing on it.
//!
//! Taking the displays and starting a shell are not the same instant. The
//! compositor modesets, and then the shell has to be spawned, link, connect,
//! bind a layer surface, create a GPU device and draw — a few hundred
//! milliseconds in which the compositor has no client to composite. What it
//! showed for that interval was `general.background`, which is very nearly
//! black, and the login screen the user was looking at a moment earlier went
//! out before the shell's wallpaper came up.
//!
//! So the compositor draws the wallpaper itself. Not an approximation of it:
//! [`lxb_protocol::wallpaper`] is the same arithmetic as the shell's shader,
//! and the display manager hands over the clock it should be evaluated at, so
//! the bridge frame and the shell's first frame are the same moment of the
//! same animation. What the user sees is one continuous wallpaper from the
//! login screen into the session, with the interface fading out at one end
//! and arriving at the other.
//!
//! # Both ends, not just the first
//!
//! It is kept for as long as the compositor runs, because the session has
//! nothing on screen at the end of a login as well as at the beginning of
//! one. The shell's surfaces go away with the process that owned them, and
//! the compositor outlives that by however long it takes to notice — and, when
//! this compositor is the one holding the login screen's own seat, that is the
//! interval between the greeter fading out and greetd starting the session.
//! Painting the clear colour into it put the black screen back at exactly the
//! boundary the rest of this exists to remove.
//!
//! Nothing has to decide when it stops standing in. The frame itself does:
//! `render` draws it only when the session contributed no element to that
//! frame, so it is under every session frame and visible in none of them.
//!
//! # It has to move
//!
//! Not because a third of a second of stillness is uncomfortable to look at,
//! but because the shell draws this same animation from this same clock.
//! Whatever the bridge is showing, the shell's first frame is the picture that
//! clock has reached by the time it arrives — so a bridge frozen at the moment
//! it was made is a picture the session jumps away from the instant it comes
//! up, by exactly as long as the freeze lasted. At the far end of a session,
//! where the wallpaper is minutes or hours along, a still made at login is not
//! a jump at all: it is a different picture.
//!
//! So a painter thread draws it, and what is on screen is the newest frame it
//! has finished. That costs what it costs — twelve milliseconds of every core
//! on the machine this was measured on, for one 640×360 frame — which is why
//! it is paced to thirty frames a second, never takes more than half of the
//! time it is running for, and runs at all only while the wallpaper is on
//! screen: the moment the session has anything of its own to show, the painter
//! stops until the session stops. The compositor's own frames cost one texture
//! upload each, and the event loop is never made to wait for a wallpaper.

use std::cell::{Cell, RefCell};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use lxb_protocol::wallpaper::{self, Sky};
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::memory::{
    MemoryRenderBuffer, MemoryRenderBufferRenderElement,
};
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::{ImportMem, Renderer};
use smithay::utils::{Buffer, Logical, Rectangle, Size, Transform};

/// How large the bridge image is drawn before it is scaled to the display.
///
/// Small, because rendering at panel resolution would add tens of milliseconds
/// to exactly the interval this is trying to shorten — a frame of it costs
/// eight times this one — and because the renderer scales textures with a
/// linear filter, which is enough for a picture with nothing sharp in it.
///
/// Almost nothing here *is* sharp: every field in the wallpaper is broad, and
/// the one thing with an edge — the band of water — is told how big a sample of
/// it is and feathers its silhouettes to match. That is what makes this size a
/// choice about cost rather than a choice about how the band looks: without it
/// a sheet pinched nearly edge-on is a couple of rows tall here, and a couple
/// of rows of hard edge scaled up threefold is a chain of beads.
const WIDTH: u32 = 640;
const HEIGHT: u32 = 360;

/// How often the painter draws a new frame while the wallpaper is on screen.
///
/// Thirty a second. The wallpaper's fastest term carries a ribbon across a
/// tenth of the screen in a second, so this is smooth and sixty would not be
/// visibly smoother — and each frame is real work on the processor of a machine
/// that is, at both the moments this runs, starting or ending a session.
const FRAME_INTERVAL: Duration = Duration::from_millis(33);

/// How long the wallpaper is painted at that rate before it is eased off, and
/// the rate it is eased off to.
///
/// The two intervals this module exists for are a third of a second each.
/// Anything longer than that is a screen with no handover coming — and there is
/// one of those on an ordinary machine: the greeter is fullscreened on one
/// display, so a second display shows this wallpaper for as long as the login
/// screen is up, which is for as long as nobody signs in. A machine idling at
/// its login screen must not be spending a third of its processor on a picture
/// of one. Five frames a second is still a moving wallpaper; it is a handover
/// it would not be smooth enough for, and by this point there is no handover.
const FULL_RATE_FOR: Duration = Duration::from_secs(10);
const EASED_INTERVAL: Duration = Duration::from_millis(200);

/// How long the painter keeps drawing after the last frame the wallpaper was
/// actually in.
///
/// Long enough to cover a few of the compositor's own frames, so a wallpaper
/// that is on screen continuously never stops being drawn between two of them,
/// and short enough that the painter is out of the way promptly once the shell
/// starts drawing — which is the moment the machine has the least attention to
/// spare.
const KEEP_PAINTING: Duration = Duration::from_millis(100);

/// How far behind its clock the frame on screen may fall before it is redrawn
/// where it stands, rather than waiting for the painter to catch up.
///
/// This is the logout case. The painter has been stopped for the whole of a
/// session, so the frame it left behind is not a moment ago — it is the picture
/// the wallpaper had at login, an hour of animation away from the one the shell
/// was showing when it exited. Waiting even one frame to correct that would put
/// a different wallpaper on screen for that frame, which is the flicker this
/// whole module exists to remove. Twelve milliseconds of the event loop, once,
/// at a logout, is the cheaper of the two.
const TOO_OLD: Duration = Duration::from_millis(250);

/// The wallpaper the compositor draws for itself, and the painter keeping it
/// at the same moment of the same animation as the shell's.
pub struct Backdrop {
    /// The frame on screen, uploaded to the renderer as it changes.
    buffer: RefCell<MemoryRenderBuffer>,
    /// The scene time that frame was drawn for, so an older frame is never
    /// put on top of a newer one.
    drawn_at: Cell<Duration>,
    painter: Arc<Painter>,
}

impl Backdrop {
    /// The whole image, as the source rectangle to scale to a display.
    ///
    /// Passing a destination size without a source rectangle would make the
    /// renderer read a display-sized region out of a 640×360 buffer, so this
    /// is not optional at the call site.
    pub fn source_size(&self) -> Size<f64, Logical> {
        (WIDTH as f64, HEIGHT as f64).into()
    }

    /// Start drawing the wallpaper for `accent`, with its clock at `scene`.
    ///
    /// `aspect` is the display's width over its height. One image serves every
    /// display: they are usually the same shape, and a bridge frame stretched
    /// across an unusual second monitor for a third of a second is still the
    /// right colours in the right places.
    pub fn start(accent: &str, style: wallpaper::Style, scene: Duration, aspect: f32) -> Self {
        let painter = Arc::new(Painter::new(accent, style, scene, aspect));
        // Drawn here, on the way up, rather than waited for: the next thing
        // this compositor does is present a frame, and there is no earlier
        // picture to show while a thread starts.
        let pixels = painter.draw(scene);
        let backdrop = Self {
            buffer: RefCell::new(MemoryRenderBuffer::from_slice(
                &pixels,
                Fourcc::Abgr8888,
                (WIDTH as i32, HEIGHT as i32),
                1,
                Transform::Normal,
                None,
            )),
            drawn_at: Cell::new(scene),
            painter: Arc::clone(&painter),
        };

        if let Err(err) = std::thread::Builder::new()
            .name("lxb-wallpaper".to_string())
            .spawn(move || paint(painter))
        {
            // A wallpaper that does not move is what this was to begin with,
            // and it is still very much better than a black screen.
            tracing::warn!(?err, "no thread to animate the wallpaper on");
        }
        backdrop
    }

    /// The wallpaper as its clock has it now, ready to be drawn over `size`.
    ///
    /// Asking for it is also what says it is on screen, because the frame that
    /// asks is the frame it goes into. That is the whole of the painter's
    /// schedule: it draws while something is drawing it, and not otherwise.
    pub fn element<R>(
        &self,
        renderer: &mut R,
        size: Size<i32, Logical>,
    ) -> Result<MemoryRenderBufferRenderElement<R>, R::Error>
    where
        R: Renderer + ImportMem,
        R::TextureId: Send + Clone + 'static,
    {
        self.catch_up();
        MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            (0.0, 0.0),
            &self.buffer.borrow(),
            None,
            Some(Rectangle::from_size(self.source_size())),
            Some(size),
            Kind::Unspecified,
        )
    }

    /// Bring the frame on screen up to the wallpaper's clock, and keep the
    /// painter drawing the next one.
    fn catch_up(&self) {
        let was_painting = self.painter.wanted_for(KEEP_PAINTING);

        let scene = self.painter.scene();
        let stale = |drawn_at: Duration| scene.saturating_sub(drawn_at) >= TOO_OLD;

        // A frame the painter left behind when it stopped is as stale as the
        // one on screen, and is dropped for the same reason.
        if let Some((at, pixels)) = self.painter.take_finished() {
            if at > self.drawn_at.get() && !stale(at) {
                self.upload(at, &pixels);
                return;
            }
        }
        // Only for a painter that had stopped. One that is merely slow has the
        // next frame coming, and drawing a wallpaper on the event loop every
        // frame because the machine is slow is how a slow machine becomes an
        // unresponsive one.
        if !was_painting && stale(self.drawn_at.get()) {
            self.upload(scene, &self.painter.draw(scene));
        }
    }

    /// Put one finished frame into the buffer the renderer uploads from.
    fn upload(&self, scene: Duration, pixels: &[u8]) {
        let mut buffer = self.buffer.borrow_mut();
        let mut context = buffer.render();
        let whole = Rectangle::from_size(Size::<i32, Buffer>::from((WIDTH as i32, HEIGHT as i32)));
        let written = context.draw(|slice| {
            if slice.len() != pixels.len() {
                return Err(());
            }
            slice.copy_from_slice(pixels);
            Ok(vec![whole])
        });
        drop(context);
        if written.is_ok() {
            self.drawn_at.set(scene);
        }
    }
}

impl Drop for Backdrop {
    fn drop(&mut self) {
        self.painter.stop();
    }
}

/// The thread that draws the wallpaper, and the frame it finished last.
///
/// It owns the clock rather than reading one: scene time is the age of
/// [`Painter::origin`], so a frame is drawn for the moment it is drawn at
/// instead of for a number counted forward from somewhere.
struct Painter {
    /// The instant this wallpaper's clock reads zero at.
    origin: Instant,
    sky: Sky,
    aspect: f32,
    work: Mutex<Work>,
    wanted: Condvar,
}

#[derive(Default)]
struct Work {
    /// The newest finished frame and the scene time it was drawn for, until
    /// a compositor frame takes it.
    finished: Option<(Duration, Vec<u8>)>,
    /// While this instant is in the future, the painter draws. Pushed forward
    /// by every frame the wallpaper is on screen in.
    until: Option<Instant>,
    /// Set when the compositor is finished with the wallpaper altogether.
    stopped: bool,
}

impl Painter {
    fn new(accent: &str, style: wallpaper::Style, scene: Duration, aspect: f32) -> Self {
        Self {
            // A scene time longer than this machine has been running cannot be
            // subtracted from now. Nothing this compositor is handed should
            // ever say so, and a wallpaper starting from zero is a great deal
            // better than a session that ends on the arithmetic.
            origin: Instant::now()
                .checked_sub(scene)
                .unwrap_or_else(Instant::now),
            sky: Sky::styled(wallpaper::palette(accent), style),
            aspect: if aspect.is_finite() && aspect > 0.0 {
                aspect
            } else {
                WIDTH as f32 / HEIGHT as f32
            },
            work: Mutex::new(Work::default()),
            wanted: Condvar::new(),
        }
    }

    /// Where the wallpaper's clock has reached.
    fn scene(&self) -> Duration {
        self.origin.elapsed()
    }

    fn draw(&self, scene: Duration) -> Vec<u8> {
        wallpaper::image(&self.sky, WIDTH, HEIGHT, self.aspect, scene.as_secs_f32())
    }

    /// A painter whose state was poisoned still has a wallpaper to draw. The
    /// only thing behind this lock is a picture.
    fn work(&self) -> MutexGuard<'_, Work> {
        self.work.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Ask for the wallpaper to go on being painted, and say whether it was
    /// being painted already.
    fn wanted_for(&self, longer: Duration) -> bool {
        let mut work = self.work();
        let was_painting = work.until.is_some_and(|until| until > Instant::now());
        work.until = Instant::now().checked_add(longer);
        drop(work);
        self.wanted.notify_one();
        was_painting
    }

    fn take_finished(&self) -> Option<(Duration, Vec<u8>)> {
        self.work().finished.take()
    }

    fn stop(&self) {
        self.work().stopped = true;
        self.wanted.notify_one();
    }
}

/// Draw frames for as long as something is putting them on screen.
fn paint(painter: Arc<Painter>) {
    // When this stretch of painting began, so a wallpaper that has been on
    // screen far longer than any handover takes can be eased off.
    let mut since = None;
    loop {
        {
            let mut work = painter.work();
            loop {
                if work.stopped {
                    return;
                }
                if work.until.is_some_and(|until| until > Instant::now()) {
                    break;
                }
                // Nothing is showing the wallpaper. That is every frame of an
                // ordinary session, which is why this is a wait and not a
                // poll: a shell drawing at sixty frames a second must not be
                // sharing the machine with a picture of what is behind it.
                since = None;
                work = painter
                    .wanted
                    .wait(work)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        }
        let started_painting = *since.get_or_insert_with(Instant::now);

        let scene = painter.scene();
        let started = Instant::now();
        let pixels = painter.draw(scene);
        let took = started.elapsed();

        {
            let mut work = painter.work();
            if work.stopped {
                return;
            }
            work.finished = Some((scene, pixels));
        }

        // Paced to the frame interval, and never running for more than half of
        // the time it takes: on a machine where a frame costs more than the
        // interval, this draws fewer of them rather than taking the processor
        // away from the session shell that is starting up beside it.
        let interval = if started_painting.elapsed() > FULL_RATE_FOR {
            EASED_INTERVAL
        } else {
            FRAME_INTERVAL
        };
        std::thread::sleep(interval.saturating_sub(took).max(took));
    }
}

/// The wallpaper this compositor will open on, before any display has said
/// what shape it is.
///
/// Kept unopened rather than drawn at start-up, for two reasons that pull the
/// same way. The clock has to be read at the moment the picture is drawn, or
/// the wallpaper opens as far behind as the compositor took to get from one to
/// the other. And the first thing that needs a picture is the first display to
/// be lit — which is also the first thing that knows what shape to draw it in.
///
/// That first commit is not a detail. A compositor lighting a connector
/// commits a frame to establish the mode, and what smithay puts in that frame
/// is whatever it was given: nothing, cleared to black. Measured on a login
/// here, that black sat on the display for the 200 ms between the session
/// compositor taking the displays and its event loop drawing anything — in the
/// middle of a hand-over the rest of this module exists to make invisible.
pub struct Opening {
    handoff: Option<std::ffi::OsString>,
}

impl Opening {
    /// What a compositor running a session opens on, and nothing otherwise: a
    /// bare `lxb` on a TTY is not standing in for a shell and has no hand-over
    /// to bridge.
    pub fn of_a_session(shell: bool, handoff: Option<&std::ffi::OsStr>) -> Option<Self> {
        shell.then(|| Self {
            handoff: handoff.map(ToOwned::to_owned),
        })
    }

    /// Draw it, for a display of this shape.
    ///
    /// The accent and the clock can come from the display manager's record; the
    /// material never does. A theme is a standing fact about the account whose
    /// session this is — the shell reads it out of `shell.toml` at startup and
    /// the login screen reads the same key — so this reads it there too rather
    /// than taking it from a hand-over that may have been written by a greeter
    /// drawing for somebody else.
    pub fn start(&self, aspect: f32) -> Backdrop {
        let (accent, style, scene) = opening_look(self.handoff.as_deref());
        tracing::info!(
            %accent,
            theme = style.name(),
            scene_secs = scene.as_secs_f32(),
            "drawing the startup wallpaper"
        );
        Backdrop::start(&accent, style, scene, aspect)
    }
}

/// What the wallpaper should look like as this session starts: the palette, the
/// material, and where the clock had reached.
///
/// The display manager's record is the good case: it carries the accent the
/// login screen was drawn in and where its clock had reached, so the bridge
/// continues that animation rather than restarting it. Without one — a plain
/// `lxb --shell` on a TTY, or a display manager that knows nothing about this —
/// the accent still comes from the user's own settings and the clock simply
/// starts at zero. Either way the screen is not black.
///
/// The material follows the same rule with one addition: the record's answer
/// where it has one, and this account's own settings otherwise. That order
/// rather than the reverse, and it is the one place in either project where a
/// hand-over outranks the file — in front of a *login screen* this compositor
/// runs as the greeter's account, and the settings it can read are not those of
/// the person whose wallpaper is on the screen. The greeter is the only process
/// that knows both, so where it has said, it is right.
///
/// One material rather than two: the shell's Theme setting has a half about the
/// wallpaper and a half about the marks it draws, and nothing here draws a mark.
/// The record's `theme` field is the wallpaper's half and has always been.
pub fn opening_look(handoff: Option<&std::ffi::OsStr>) -> (String, wallpaper::Style, Duration) {
    match handoff.and_then(|record| record.to_str()).and_then(read) {
        Some((accent, style, scene)) => {
            (accent, style.unwrap_or_else(|| configured_look().1), scene)
        }
        None => {
            let (accent, style) = configured_look();
            (accent, style, Duration::ZERO)
        }
    }
}

/// Read the accent and the continuing clock out of a display manager's record.
///
/// Deliberately incomplete: the shell owns this record's meaning and does the
/// whole of its validation, including the checks that matter for trusting it —
/// that it came from this boot and has not gone stale. This reads the fields a
/// picture needs and gives up on anything it does not recognise. A record this
/// rejects is still passed on to the shell untouched.
///
/// `visual` is the exception, and it is not one of the shell's checks being
/// duplicated for its own sake: it is the field that says whether the number
/// beside it is a phase of *this* scene at all. A display manager still drawing
/// an older wallpaper hands over a time the shell will refuse, and a bridge
/// frame painted from it would put a picture on screen that the shell's own
/// first frame then jumps away from — which is the seam this whole module
/// exists to remove. Refused here, both ends of that boot start the animation
/// from zero together.
fn read(record: &str) -> Option<(String, Option<wallpaper::Style>, Duration)> {
    // The same bound the shell applies, so a malformed environment cannot
    // make the compositor walk a long string before the session starts.
    if record.len() > 1024 || !record.is_ascii() {
        return None;
    }

    let mut accent = None;
    let mut sample_ns = None;
    let mut scene_ns = None;
    let mut visual = None;
    let mut theme = None;
    for field in record.split(';') {
        let (key, value) = field.split_once('=')?;
        match key {
            "accent" => accent = Some(value),
            "visual" => visual = Some(value),
            "theme" => theme = Some(value),
            "sample-ns" => sample_ns = Some(value.parse::<u64>().ok()?),
            "scene-ns" => scene_ns = Some(value.parse::<u64>().ok()?),
            _ => {}
        }
    }

    // The scene this build draws, or no scene time at all.
    if visual? != wallpaper::VISUAL {
        return None;
    }

    let accent = accent?;
    let palette = wallpaper::palette(accent);
    if palette.name != accent {
        return None;
    }

    // Advance the handed-over scene time by however long the handover itself
    // took, from the same clock the display manager sampled. This is the
    // whole point of the record: the wallpaper carries on from where it was
    // rather than from where it was a login ago.
    let elapsed_ns = monotonic_now_ns()?.checked_sub(sample_ns?)?;
    let scene_ns = scene_ns?.checked_add(elapsed_ns)?;
    Some((
        accent.to_string(),
        theme.map(wallpaper::style),
        Duration::from_nanos(scene_ns),
    ))
}

fn monotonic_now_ns() -> Option<u64> {
    let mut timestamp = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `timestamp` points to a live `timespec`, and CLOCK_MONOTONIC
    // needs no further ownership or lifetime guarantees.
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut timestamp) } != 0 {
        return None;
    }
    let seconds = u64::try_from(timestamp.tv_sec).ok()?;
    let nanos = u64::try_from(timestamp.tv_nsec).ok()?;
    seconds.checked_mul(1_000_000_000)?.checked_add(nanos)
}

/// The accent the user's shell is set to, read straight from its settings.
///
/// The compositor does not own this setting and does not interpret the rest
/// of that file; it reads one bounded value so a session started without a
/// display manager still comes up in the right colour. An unreadable or
/// unrecognised value falls back to the default palette, which is what the
/// shell itself does with it.
/// The accent and the material the shell is set to, out of its own settings
/// file.
///
/// Both keys, in one read, because they are one answer: a bridge frame in the
/// shell's accent but the wrong material is as much of a seam as one in the
/// wrong colour. The theme is the shell's own — see `wallpaper::Style` — and a
/// name this build does not know is read as the shell reads it, which is the
/// default one.
///
/// The *wallpaper's* half of the theme, and only that half. The shell's Theme
/// setting is two: what the picture behind everything is made of, and what its
/// own marks are made of. This compositor draws a wallpaper and no marks at all,
/// so the second key is none of its business. `theme` is what both halves were
/// written under before they were split, and is read where the wallpaper has no
/// key of its own — a machine set to `Simple` before an update must not come up
/// in the water for the seconds before the shell's first frame.
fn configured_look() -> (String, wallpaper::Style) {
    let default = (
        wallpaper::PALETTES[0].name.to_string(),
        wallpaper::Style::default(),
    );
    let Some(path) = shell_settings_path() else {
        return default;
    };
    let Ok(metadata) = std::fs::metadata(&path) else {
        return default;
    };
    // A settings file is a few hundred bytes. Anything of this size is not
    // one, and reading it before the session starts is not worth the wait.
    if metadata.len() > 256 * 1024 {
        return default;
    }
    let Ok(text) = std::fs::read_to_string(&path) else {
        return default;
    };
    let Ok(table) = text.parse::<toml::Table>() else {
        return default;
    };
    look_in(&table)
}

/// The same two answers, out of a parsed settings file.
///
/// Split from [`configured_look`] so the keys can be exercised without the
/// environment this compositor is normally started in: everything above this is
/// about finding a file and refusing to read one that is not a settings file.
fn look_in(table: &toml::Table) -> (String, wallpaper::Style) {
    let accent = table
        .get("accent")
        .and_then(toml::Value::as_str)
        .filter(|name| wallpaper::palette(name).name == *name)
        .map(str::to_string)
        .unwrap_or_else(|| wallpaper::PALETTES[0].name.to_string());
    let style = table
        .get("theme-wallpaper")
        .or_else(|| table.get("theme"))
        .and_then(toml::Value::as_str)
        .map(wallpaper::style)
        .unwrap_or_default();
    (accent, style)
}

fn shell_settings_path() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| Some(std::path::PathBuf::from(std::env::var_os("HOME")?).join(".config")))?;
    Some(base.join("lxb").join("shell.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Keep this fixture byte-for-byte identical to the display manager's
    /// canonical encoder, and to the shell's consumer fixture.
    fn record(accent: &str, sample_ns: u64, scene_ns: u64) -> String {
        format!(
            "v=1;visual=lxb-wallpaper-v2;clock=linux-monotonic;\
             boot=01234567-89ab-cdef-0123-456789abcdef;\
             sample-ns={sample_ns};scene-ns={scene_ns};accent={accent}"
        )
    }

    #[test]
    fn a_records_accent_and_clock_reach_the_bridge_frame() {
        let sample_ns = monotonic_now_ns().expect("monotonic clock");
        let (accent, style, scene) =
            read(&record("Green", sample_ns, 42_000_000_000)).expect("record");
        // A record that says nothing about the material leaves the question to
        // this account's own settings, which is what `opening_look` then asks.
        assert_eq!(style, None);
        assert_eq!(accent, "Green");
        // At least the handed-over scene time, plus however long this test
        // took to get here — never less, which would run the clock backwards.
        assert!(scene >= Duration::from_secs(42));
        assert!(scene < Duration::from_secs(43));
    }

    /// The material the login screen was drawing in reaches the bridge frame in
    /// front of it.
    ///
    /// This is the one thing the record says that this compositor cannot look up
    /// for itself. In front of a login screen it runs as the greeter's own
    /// account, and the settings of the person whose wallpaper is on the screen
    /// are in a home directory it has no business reading — so a greeter drawing
    /// the plain material has to be able to say so, or the two of them disagree
    /// for the third of a second the bridge is up.
    #[test]
    fn the_material_the_greeter_drew_in_reaches_the_bridge_frame() {
        let sample_ns = monotonic_now_ns().expect("monotonic clock");
        let plain = format!(
            "{};theme=Simple",
            record("Purple", sample_ns, 42_000_000_000)
        );
        let (_, style, _) = read(&plain).expect("a record with a material is still a record");
        assert_eq!(style, Some(wallpaper::Style::Simple));
        assert_eq!(
            opening_look(Some(std::ffi::OsStr::new(&plain))).1,
            wallpaper::Style::Simple,
            "what the greeter said, not what this account's file says"
        );

        // A name this build does not know is read the way the shell reads one:
        // as its own look, rather than as a reason to throw the phase away.
        let odd = format!(
            "{};theme=Glass",
            record("Purple", sample_ns, 42_000_000_000)
        );
        let (_, style, _) = read(&odd).expect("an unknown material is not a broken record");
        assert_eq!(style, Some(wallpaper::Style::Default));
    }

    /// The wallpaper's half of the shell's Theme setting, and the key both
    /// halves shared before there were two of them.
    ///
    /// This compositor draws a wallpaper and never a mark, so `theme-icons` is
    /// none of its business and must not reach the bridge frame. The old `theme`
    /// key is read where the wallpaper has nothing of its own, because a machine
    /// stood down to `Simple` before the split meant it about the picture too.
    #[test]
    fn the_bridge_frame_reads_the_wallpapers_half_of_the_theme() {
        let look = |settings: &str| look_in(&settings.parse().expect("a settings file"));

        assert_eq!(look("").1, wallpaper::Style::Default);
        assert_eq!(
            look("theme-wallpaper = \"Simple\"").1,
            wallpaper::Style::Simple
        );
        assert_eq!(
            look("theme = \"Simple\"").1,
            wallpaper::Style::Simple,
            "a file from before the split still says what it said"
        );
        assert_eq!(
            look("theme = \"Simple\"\ntheme-wallpaper = \"Default\"").1,
            wallpaper::Style::Default,
            "and the newer, narrower key outranks it"
        );
        assert_eq!(
            look("theme-icons = \"Simple\"").1,
            wallpaper::Style::Default,
            "the marks are the shell's own business, and nothing here draws one"
        );
        assert_eq!(
            look("accent = \"Green\"\ntheme-wallpaper = \"Simple\""),
            ("Green".to_string(), wallpaper::Style::Simple),
            "both keys, in one read: they are one answer about one frame"
        );
    }

    /// A user whose shell stands their own picture behind everything has one
    /// thing this compositor has not: the file. It is under their home, and the
    /// bridge frame is drawn before any of that is open — by a process that is
    /// deliberately not in the business of reading a user's pictures.
    ///
    /// So the key is read, recognised, and drawn as the shell's own scene. What
    /// must not happen is the frame coming up in `Simple`, or the key being
    /// refused and the whole record with it.
    #[test]
    fn a_custom_wallpaper_bridges_with_the_shells_own_scene() {
        let look = |settings: &str| look_in(&settings.parse().expect("a settings file"));

        let style = look(&format!("theme-wallpaper = \"{}\"", wallpaper::CUSTOM)).1;
        assert_eq!(style, wallpaper::Style::Custom);
        assert_eq!(style.analytic(), wallpaper::Style::Default);
    }

    /// A display manager on the older wallpaper is handing over a phase of a
    /// picture nothing here draws any more. The shell refuses that record, so
    /// the bridge frame in front of it has to refuse it too — the two of them
    /// starting from different moments of the animation is exactly the seam
    /// this module removes.
    #[test]
    fn a_record_for_another_wallpaper_is_declined() {
        let sample_ns = monotonic_now_ns().expect("monotonic clock");
        let retired = record("Purple", sample_ns, 42_000_000_000)
            .replace(wallpaper::VISUAL, "lxb-wallpaper-v1");
        assert!(read(&retired).is_none());
        // And the identifier is being compared, not merely present.
        assert!(read(&record("Purple", sample_ns, 42_000_000_000)).is_some());
    }

    #[test]
    fn an_unusable_record_is_declined_rather_than_guessed_at() {
        let sample_ns = monotonic_now_ns().expect("monotonic clock");
        assert!(read(&record("Chartreuse", sample_ns, 1)).is_none());
        assert!(read(&record("green", sample_ns, 1)).is_none());
        assert!(read(&record("Green", sample_ns, 1).replace("scene-ns=1", "")).is_none());
        assert!(read("nonsense").is_none());
        assert!(read(&"x".repeat(1025)).is_none());
        // A sample from the future would run the wallpaper's clock backwards.
        assert!(read(&record("Green", u64::MAX, 1)).is_none());
    }

    /// The one thing the compositor must never do with this record is change
    /// it: the shell does the real validation, and it has to see exactly what
    /// the display manager wrote.
    #[test]
    fn reading_a_record_leaves_it_alone() {
        let sample_ns = monotonic_now_ns().expect("monotonic clock");
        let original = record("Blue", sample_ns, 5);
        let _ = read(&original);
        assert_eq!(original, record("Blue", sample_ns, 5));
    }

    #[test]
    fn a_session_with_no_record_still_gets_a_palette_and_a_clock() {
        let (accent, style, scene) = opening_look(None);
        assert_eq!(wallpaper::palette(&accent).name, accent);
        // And a material, which without a record is whatever this account's own
        // settings say — the shell's own look on a machine that has not asked
        // for anything else.
        assert!(wallpaper::STYLES.contains(&style.name()));
        assert_eq!(scene, Duration::ZERO);
    }

    /// A display whose size the compositor could not read yet must not turn
    /// the bridge frame into transparent or `NaN` pixels — that would be the
    /// black screen back again, by another route.
    #[test]
    fn a_nonsensical_aspect_still_draws_a_wallpaper() {
        let sky = Sky::new(wallpaper::palette("Purple"));
        for aspect in [f32::NAN, 0.0, -2.0, f32::INFINITY] {
            let backdrop = Backdrop::start(
                "Purple",
                wallpaper::Style::Default,
                Duration::from_secs(3),
                aspect,
            );
            assert_eq!(backdrop.source_size(), (WIDTH as f64, HEIGHT as f64).into());
        }
        // And the fallback shape is the one a sane aspect would have drawn.
        let fallback = wallpaper::image(&sky, 8, 8, WIDTH as f32 / HEIGHT as f32, 3.0);
        assert!(fallback.chunks_exact(4).all(|pixel| pixel[3] == 0xff));
    }

    /// The picture on screen follows the clock, which is the whole of the
    /// reason this animates: the shell's first frame is drawn from the same
    /// clock, and it arrives at whatever that clock says by then.
    #[test]
    fn the_frame_on_screen_follows_the_wallpaper_clock() {
        let backdrop = Backdrop::start(
            "Purple",
            wallpaper::Style::Default,
            Duration::ZERO,
            16.0 / 9.0,
        );

        // Wait for two frames rather than one, and time the second: what the
        // frame on screen may be behind the clock is what the painter *can* do
        // here, and this is the machine saying so rather than this test
        // assuming it. The wallpaper is thirty-odd transcendental functions a
        // pixel, and a run of the whole suite has every core already busy with
        // the other thousand tests — a frame that honestly took a third of a
        // second to paint is still the newest frame there is. What the test is
        // for is a painter that hands over a frame it had been sitting on,
        // which is cycles behind rather than one.
        let mut seen = Vec::new();
        let mut cycle = None;
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut since = Instant::now();
        while seen.len() < 2 && Instant::now() < deadline {
            backdrop.catch_up();
            let drawn_at = backdrop.drawn_at.get();
            if seen.last() != Some(&drawn_at) {
                cycle = Some(since.elapsed());
                since = Instant::now();
                seen.push(drawn_at);
            }
            std::thread::sleep(Duration::from_millis(4));
        }

        let (Some([first, second]), Some(cycle)) = (seen.first_chunk::<2>(), cycle) else {
            panic!("the wallpaper never moved past the frame it started on");
        };
        assert!(second > first, "{second:?} is not past {first:?}");
        // And what it moved to is the moment it was shown at, not some frame
        // the painter had been sitting on.
        let behind = backdrop.painter.scene().saturating_sub(*second);
        let allowed = TOO_OLD + cycle * 2;
        assert!(
            behind < allowed,
            "{behind:?} behind the clock, and a frame here takes {cycle:?}"
        );
    }

    /// Nothing is painted while the session has the screen. A shell drawing at
    /// sixty frames a second must not be sharing the machine with a picture of
    /// what is behind it.
    #[test]
    fn the_painter_stops_when_the_wallpaper_leaves_the_screen() {
        let backdrop = Backdrop::start(
            "Blue",
            wallpaper::Style::Default,
            Duration::ZERO,
            16.0 / 9.0,
        );
        backdrop.catch_up();

        // Long enough for the grace period to pass and for whatever was
        // already being drawn when it did to be finished and thrown away.
        std::thread::sleep(KEEP_PAINTING + Duration::from_millis(400));
        let _ = backdrop.painter.take_finished();
        std::thread::sleep(Duration::from_millis(400));

        assert!(
            backdrop.painter.take_finished().is_none(),
            "the wallpaper was still being painted with the session on screen"
        );
    }

    /// Drawing a frame where it stands is for a painter that had stopped, and
    /// never for one that is only slow: a machine where a frame costs more
    /// than the interval must not be made to draw one on its event loop at
    /// every frame on top of that.
    #[test]
    fn only_a_stopped_painter_is_woken_by_the_frame_that_needs_it() {
        let backdrop =
            Backdrop::start("Red", wallpaper::Style::Default, Duration::ZERO, 16.0 / 9.0);
        // The first frame a wallpaper comes back on screen in.
        assert!(!backdrop.painter.wanted_for(KEEP_PAINTING));
        // And every frame after it, for as long as it stays there.
        assert!(backdrop.painter.wanted_for(KEEP_PAINTING));
    }

    /// The logout case: the painter has been stopped for the whole of a
    /// session, so the frame it left behind is not a moment ago. It must not
    /// reach the screen even once.
    #[test]
    fn a_frame_left_over_from_a_session_ago_is_never_shown() {
        let backdrop = Backdrop::start(
            "Green",
            wallpaper::Style::Default,
            Duration::from_secs(3600),
            16.0 / 9.0,
        );
        // As if the shell had had the screen for an hour.
        let long_ago = backdrop.painter.scene() - Duration::from_secs(3600);
        backdrop.drawn_at.set(long_ago);
        backdrop
            .painter
            .work()
            .finished
            .replace((long_ago, backdrop.painter.draw(long_ago)));

        // The clock as the correction starts, which is the moment the frame
        // that reaches the screen has to be for. Read here and not afterwards:
        // `catch_up` samples the clock once and stamps the frame it draws with
        // that sample, so reading the clock again at the end would be
        // measuring how long this machine takes to paint a wallpaper — nearly
        // two seconds in a debug build, on top of whatever else the suite has
        // the cores doing — and not whether the frame was stale.
        let began = backdrop.painter.scene();
        backdrop.catch_up();

        let behind = began.saturating_sub(backdrop.drawn_at.get());
        assert!(
            behind < TOO_OLD,
            "an hour-old wallpaper went to the screen: {behind:?} behind the clock"
        );
    }
}
