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
/// Every term in the wallpaper is broad — the finest of them, a silk ribbon's
/// crest, is still several rows tall here — and the renderer scales textures
/// with a linear filter. Rendering at panel resolution instead would add tens
/// of milliseconds to exactly the interval this is trying to shorten.
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
    pub fn start(accent: &str, scene: Duration, aspect: f32) -> Self {
        let painter = Arc::new(Painter::new(accent, scene, aspect));
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
    fn new(accent: &str, scene: Duration, aspect: f32) -> Self {
        Self {
            // A scene time longer than this machine has been running cannot be
            // subtracted from now. Nothing this compositor is handed should
            // ever say so, and a wallpaper starting from zero is a great deal
            // better than a session that ends on the arithmetic.
            origin: Instant::now()
                .checked_sub(scene)
                .unwrap_or_else(Instant::now),
            sky: Sky::new(wallpaper::palette(accent)),
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
    pub fn start(&self, aspect: f32) -> Backdrop {
        let (accent, scene) = opening_scene(self.handoff.as_deref());
        tracing::info!(
            %accent,
            scene_secs = scene.as_secs_f32(),
            "drawing the startup wallpaper"
        );
        Backdrop::start(&accent, scene, aspect)
    }
}

/// What the wallpaper should look like as this session starts.
///
/// The display manager's record is the good case: it carries both the accent
/// the login screen was drawn in and where its clock had reached, so the
/// bridge continues that animation rather than restarting it. Without one —
/// a plain `lxb --shell` on a TTY, or a display manager that knows nothing
/// about this — the accent still comes from the user's own settings and the
/// clock simply starts at zero. Either way the screen is not black.
pub fn opening_scene(handoff: Option<&std::ffi::OsStr>) -> (String, Duration) {
    match handoff.and_then(|record| record.to_str()).and_then(read) {
        Some(scene) => scene,
        None => (configured_accent(), Duration::ZERO),
    }
}

/// Read the accent and the continuing clock out of a display manager's record.
///
/// Deliberately incomplete: the shell owns this record's meaning and does the
/// whole of its validation, including the checks that matter for trusting it —
/// that it came from this boot and has not gone stale. This reads the two
/// fields a picture needs and gives up on anything it does not recognise. A
/// record this rejects is still passed on to the shell untouched.
fn read(record: &str) -> Option<(String, Duration)> {
    // The same bound the shell applies, so a malformed environment cannot
    // make the compositor walk a long string before the session starts.
    if record.len() > 1024 || !record.is_ascii() {
        return None;
    }

    let mut accent = None;
    let mut sample_ns = None;
    let mut scene_ns = None;
    for field in record.split(';') {
        let (key, value) = field.split_once('=')?;
        match key {
            "accent" => accent = Some(value),
            "sample-ns" => sample_ns = Some(value.parse::<u64>().ok()?),
            "scene-ns" => scene_ns = Some(value.parse::<u64>().ok()?),
            _ => {}
        }
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
    Some((accent.to_string(), Duration::from_nanos(scene_ns)))
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
fn configured_accent() -> String {
    let default = wallpaper::PALETTES[0].name.to_string();
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
    text.parse::<toml::Table>()
        .ok()
        .and_then(|table| {
            let name = table.get("accent")?.as_str()?;
            (wallpaper::palette(name).name == name).then(|| name.to_string())
        })
        .unwrap_or(default)
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
            "v=1;visual=lxb-wallpaper-v1;clock=linux-monotonic;\
             boot=01234567-89ab-cdef-0123-456789abcdef;\
             sample-ns={sample_ns};scene-ns={scene_ns};accent={accent}"
        )
    }

    #[test]
    fn a_records_accent_and_clock_reach_the_bridge_frame() {
        let sample_ns = monotonic_now_ns().expect("monotonic clock");
        let (accent, scene) = read(&record("Green", sample_ns, 42_000_000_000)).expect("record");
        assert_eq!(accent, "Green");
        // At least the handed-over scene time, plus however long this test
        // took to get here — never less, which would run the clock backwards.
        assert!(scene >= Duration::from_secs(42));
        assert!(scene < Duration::from_secs(43));
    }

    #[test]
    fn an_unusable_record_is_declined_rather_than_guessed_at() {
        let sample_ns = monotonic_now_ns().expect("monotonic clock");
        assert!(read(&record("Orange", sample_ns, 1)).is_none());
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
        let (accent, scene) = opening_scene(None);
        assert_eq!(wallpaper::palette(&accent).name, accent);
        assert_eq!(scene, Duration::ZERO);
    }

    /// A display whose size the compositor could not read yet must not turn
    /// the bridge frame into transparent or `NaN` pixels — that would be the
    /// black screen back again, by another route.
    #[test]
    fn a_nonsensical_aspect_still_draws_a_wallpaper() {
        let sky = Sky::new(wallpaper::palette("Purple"));
        for aspect in [f32::NAN, 0.0, -2.0, f32::INFINITY] {
            let backdrop = Backdrop::start("Purple", Duration::from_secs(3), aspect);
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
        let backdrop = Backdrop::start("Purple", Duration::ZERO, 16.0 / 9.0);
        let first = backdrop.drawn_at.get();

        // Every pass is one compositor frame asking for the wallpaper. A
        // painted frame costs milliseconds, so this is a handful of passes on
        // any machine and the wait is only there to bound a broken one.
        let deadline = Instant::now() + Duration::from_secs(5);
        while backdrop.drawn_at.get() == first && Instant::now() < deadline {
            backdrop.catch_up();
            std::thread::sleep(Duration::from_millis(4));
        }

        assert!(
            backdrop.drawn_at.get() > first,
            "the wallpaper never moved past the frame it started on"
        );
        // And what it moved to is the moment it was shown at, not some frame
        // the painter had been sitting on.
        let behind = backdrop
            .painter
            .scene()
            .saturating_sub(backdrop.drawn_at.get());
        assert!(behind < TOO_OLD, "{behind:?} behind the clock");
    }

    /// Nothing is painted while the session has the screen. A shell drawing at
    /// sixty frames a second must not be sharing the machine with a picture of
    /// what is behind it.
    #[test]
    fn the_painter_stops_when_the_wallpaper_leaves_the_screen() {
        let backdrop = Backdrop::start("Blue", Duration::ZERO, 16.0 / 9.0);
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
        let backdrop = Backdrop::start("Red", Duration::ZERO, 16.0 / 9.0);
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
        let backdrop = Backdrop::start("Green", Duration::from_secs(3600), 16.0 / 9.0);
        // As if the shell had had the screen for an hour.
        let long_ago = backdrop.painter.scene() - Duration::from_secs(3600);
        backdrop.drawn_at.set(long_ago);
        backdrop
            .painter
            .work()
            .finished
            .replace((long_ago, backdrop.painter.draw(long_ago)));

        backdrop.catch_up();

        let behind = backdrop
            .painter
            .scene()
            .saturating_sub(backdrop.drawn_at.get());
        assert!(
            behind < TOO_OLD,
            "an hour-old wallpaper went to the screen: {behind:?} behind the clock"
        );
    }
}
