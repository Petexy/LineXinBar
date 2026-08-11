//! The white a display gives when it has just been photographed.
//!
//! A screenshot taken from a key is the one thing the session does that leaves
//! no trace on screen. The shell cannot answer for it either: while a
//! fullscreen application is up the shell's own surface is behind it, so a
//! panel saying "saved" would be a panel nobody could see — and one that rose
//! in front of the game to be seen would be worse than saying nothing at all.
//!
//! What is left is the display itself, which the compositor draws. So the
//! screen flashes, the way every camera anyone has held answers a shutter: a
//! fast rise to white and a slower fade off it, over the whole display and over
//! everything on it.
//!
//! It is started *after* the picture has been read back, so the flash is never
//! in the photograph — and only when the picture was actually written, so the
//! flash means the screenshot happened rather than merely that a key was
//! pressed.
//!
//! Nothing here is ticked: like the overview and the restore flight, the
//! brightness is a pure function of the clock, sampled once per rendered frame.

use std::time::{Duration, Instant};

use smithay::output::Output;

/// How long the screen takes to reach full white.
///
/// Short enough to read as a shutter rather than as a fade to white, long
/// enough not to be a single frame appearing from nowhere: three frames at
/// 60 Hz.
const RISE: Duration = Duration::from_millis(50);

/// And how long it takes to come back off it, which is the part the eye
/// actually reads as a camera.
const FALL: Duration = Duration::from_millis(250);

/// How white the whitest frame is.
///
/// Not 1.0. A screen that goes completely white loses the picture underneath
/// it, and the picture underneath it is what was just photographed — the flash
/// should confirm what is on the screen, not replace it.
const PEAK: f32 = 0.72;

/// One display's flash.
#[derive(Debug, Clone)]
struct Flash {
    /// By name, because a display can go away and come back underneath an
    /// animation, and a name compares equal to the display that returns.
    output: String,
    /// Stable for the life of the flash, so the damage tracker sees one
    /// element changing brightness rather than a new element every frame.
    id: smithay::backend::renderer::element::Id,
    started: Instant,
}

/// Every display currently flashing.
#[derive(Debug, Default)]
pub struct Flashes {
    flashes: Vec<Flash>,
}

impl Flashes {
    /// Flash `output`, from now.
    ///
    /// A display already flashing starts again rather than gaining a second
    /// flash: two screenshots in quick succession are two pictures and one
    /// shutter, and stacking the whites would take the screen to a brighter
    /// place than either of them asked for.
    pub fn begin(&mut self, output: &Output, now: Instant) {
        self.flashes.retain(|flash| flash.output != output.name());
        self.flashes.push(Flash {
            output: output.name(),
            id: smithay::backend::renderer::element::Id::new(),
            started: now,
        });
    }

    /// How white `output` is at `now`, and which element that white belongs to.
    /// `None` when this display is not flashing.
    pub fn white(
        &self,
        output: &Output,
        now: Instant,
    ) -> Option<(smithay::backend::renderer::element::Id, f32)> {
        let flash = self
            .flashes
            .iter()
            .find(|flash| flash.output == output.name())?;
        let alpha = alpha(now.saturating_duration_since(flash.started))?;
        Some((flash.id.clone(), alpha))
    }

    /// Drop the flashes that have burnt out. Called from the render pass, which
    /// is the only thing that samples them.
    pub fn prune(&mut self, now: Instant) {
        self.flashes
            .retain(|flash| alpha(now.saturating_duration_since(flash.started)).is_some());
    }
}

/// The curve: up over [`RISE`], down over [`FALL`], smoothstepped at both ends
/// so neither edge is a jump. `None` once there is nothing left to draw.
fn alpha(since: Duration) -> Option<f32> {
    if since < RISE {
        return Some(PEAK * smoothstep(since.as_secs_f32() / RISE.as_secs_f32()));
    }
    let falling = (since - RISE).as_secs_f32() / FALL.as_secs_f32();
    if falling >= 1.0 {
        return None;
    }
    Some(PEAK * (1.0 - smoothstep(falling)))
}

fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output(name: &str) -> Output {
        Output::new(
            name.to_string(),
            smithay::output::PhysicalProperties {
                size: (0, 0).into(),
                subpixel: smithay::output::Subpixel::Unknown,
                make: "test".into(),
                model: "test".into(),
            },
        )
    }

    /// It rises to white and comes back off it, and then it is over.
    #[test]
    fn a_photographed_display_flashes_and_stops() {
        let out = output("A");
        let mut flashes = Flashes::default();
        let t0 = Instant::now();
        flashes.begin(&out, t0);

        let (_, start) = flashes.white(&out, t0).expect("flashing from the start");
        assert!(start < PEAK / 4.0, "the rise starts dark: {start}");
        assert_eq!(flashes.white(&out, t0 + RISE).unwrap().1, PEAK);
        let (_, half) = flashes.white(&out, t0 + RISE + FALL / 2).unwrap();
        assert!(
            half > 0.0 && half < PEAK,
            "somewhere on the way down: {half}"
        );
        assert!(
            flashes.white(&out, t0 + RISE + FALL).is_none(),
            "a flash that never ended would be a white screen"
        );
    }

    /// Neither edge is a jump, and neither is a straight line.
    #[test]
    fn the_flash_is_eased_at_both_ends() {
        let quarter = alpha(RISE / 4).unwrap();
        assert!(
            quarter < PEAK / 4.0,
            "the rise starts slowly, not linearly: {quarter}"
        );
        let nearly_out = alpha(RISE + FALL * 3 / 4).unwrap();
        assert!(
            nearly_out < PEAK / 4.0,
            "and it settles out of white: {nearly_out}"
        );
        assert_eq!(alpha(RISE + FALL / 2), Some(PEAK / 2.0));
    }

    /// The screen never goes entirely white: what was photographed stays
    /// readable underneath the answer that it was.
    #[test]
    fn the_picture_shows_through_the_whitest_frame() {
        const { assert!(PEAK < 1.0) };
        for step in 0..=20 {
            let at = (RISE + FALL).mul_f32(step as f32 / 20.0);
            assert!(alpha(at).unwrap_or(0.0) <= PEAK);
        }
    }

    /// A flash belongs to the display it was taken on, and a second picture is
    /// one shutter rather than two whites laid on top of each other.
    #[test]
    fn a_flash_belongs_to_one_display_and_does_not_stack() {
        let (a, b) = (output("A"), output("B"));
        let mut flashes = Flashes::default();
        let t0 = Instant::now();
        flashes.begin(&a, t0);
        assert!(flashes.white(&a, t0).is_some());
        assert!(flashes.white(&b, t0).is_none(), "one display, not the pair");

        flashes.begin(&a, t0 + RISE);
        assert_eq!(flashes.flashes.len(), 1, "one display, one flash");
        // The clock restarted with it, so it is at the bottom of the rise.
        assert!(flashes.white(&a, t0 + RISE).unwrap().1 < PEAK);
    }

    /// Burnt-out flashes are dropped, so a session that has taken a thousand
    /// screenshots is not carrying a thousand of these.
    #[test]
    fn spent_flashes_are_forgotten() {
        let out = output("A");
        let mut flashes = Flashes::default();
        let t0 = Instant::now();
        flashes.begin(&out, t0);
        flashes.prune(t0 + RISE);
        assert_eq!(flashes.flashes.len(), 1, "still flashing");
        flashes.prune(t0 + RISE + FALL);
        assert!(flashes.flashes.is_empty());
    }
}
