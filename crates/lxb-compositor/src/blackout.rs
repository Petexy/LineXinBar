//! The black a screen rests behind while somebody is using another one.
//!
//! An OLED panel keeps what it is shown. The start screen is the worst thing
//! there is to keep: the bar sits in the same row of pixels every second it is
//! up, the clock in the same corner, and a second display left on it through an
//! evening's play is a display with a bar burnt into it. Nothing about that is
//! hypothetical, and nothing about the screen being *nearly* still helps —
//! still is what does the damage.
//!
//! So a display the user has left alone is taken down to black, and brought
//! back the moment they go near it. What decides *when* is the shell's, up in
//! its Settings column: the setting is per display, the rule is written down in
//! `lxb_shell_v1.cover_output_in_black`, and none of it is here. What is here
//! is the sheet.
//!
//! It is drawn by the compositor for the reason [`crate::curtain`]'s is: the
//! shell owns one surface per display and nothing else on it. A shell blacking
//! out its own surfaces would rest a screen and leave a paused film lit on it,
//! with the cursor over the top.
//!
//! ## What this is not
//!
//! It is not the curtain. The session goes on taking input the whole time one
//! of these is down — on the resting display and on every other — because the
//! way out of it is the user moving the pointer onto the screen they want back,
//! and a sheet that swallowed that movement could never be lifted by it.
//!
//! It is not a display being switched off, either. Turning a connector off is a
//! modeset to get back out of, which on a television is a second of black and a
//! resync; black pixels on an OLED panel are already an unlit panel, which is
//! the whole of what this is for.
//!
//! ## What is under it
//!
//! Once a sheet is all the way down, nothing on that display can be seen, and
//! it is treated exactly as a display something else covers: its windows are
//! off screen for [`crate::render::windows_on_screen`], so they are sent no
//! frames and the applications they belong to are stopped by [`crate::sleep`]
//! until the sheet is asked back up — which the shell does on input, and which
//! continues them before the first frame of the way up. See
//! [`Blackouts::is_black`]. A window arriving on a black display is still let
//! run, and what it paints wakes the display the way painting always has.
//!
//! ## Frames
//!
//! Like the flash and the curtain, how black a display is now is a pure
//! function of the clock, sampled once per rendered frame. The commit counter
//! moves with it and only with it — a solid colour whose commit never changes
//! is one the damage tracker leaves on screen exactly as it first drew it, and
//! a counter that went on moving after the fade would keep a resting display
//! redrawing a black rectangle for the rest of the evening.

use std::time::{Duration, Instant};

use smithay::backend::renderer::element::Id;
use smithay::backend::renderer::utils::CommitCounter;
use smithay::output::Output;

/// How long a display takes to go black.
///
/// Twice the curtain's, and slower on purpose. Nobody is meant to be looking at
/// this one — it only happens once a screen has been left alone — so what the
/// length is for is the corner of the eye it *is* caught by: a second reads as
/// a screen settling, and half of that reads as a screen switching off.
const DOWN: Duration = Duration::from_millis(1000);

/// And how long it takes to come back.
///
/// A quarter of a second, because by the time this runs the user has already
/// asked for the screen — they have moved the pointer onto it, or taken the
/// display over — and everything after the asking is delay. It is short rather
/// than instant only so that the picture arrives rather than appears.
const UP: Duration = Duration::from_millis(250);

/// One display's sheet, and how far it has got.
#[derive(Debug)]
struct Sheet {
    /// By name, because a display can go away and come back underneath a fade,
    /// and a name compares equal to the display that returns.
    output: String,
    /// Stable for the life of the sheet, so the damage tracker sees one element
    /// changing rather than a new element every frame.
    id: Id,
    /// When it started moving, and which way.
    started: Instant,
    down: bool,
    /// How black it was when it started moving that way. A sheet turned round
    /// halfway carries on from where it is rather than jumping to the far end
    /// and running back — which, unlike the curtain, is the ordinary case here
    /// and not the exceptional one: coming back is what every one of these is
    /// eventually asked to do.
    from: f32,
    /// What the commit counter had reached when this leg started, so the counter
    /// this element reports only ever goes up. A sheet turned round three times
    /// in a minute would otherwise hand the damage tracker the same numbers over
    /// again, which is a fade it has already seen.
    commits: usize,
}

/// Every display currently resting, or on its way in or out of it.
#[derive(Debug, Default)]
pub struct Blackouts {
    sheets: Vec<Sheet>,
}

impl Blackouts {
    /// Take `output` down to black, or bring it back, from `now`.
    ///
    /// Asking for what is already happening is ignored rather than restarted,
    /// so a shell that repeats itself does not put the fade back to the
    /// beginning — and neither does one that says so once a frame, which is how
    /// a value the shell holds is ordinarily kept in step.
    pub fn cover(&mut self, output: &Output, covered: bool, now: Instant) {
        let name = output.name();
        let existing = self.sheets.iter().position(|sheet| sheet.output == name);
        match existing {
            Some(index) if self.sheets[index].down == covered => return,
            // Nothing to bring back. There is no sheet at all for a display
            // nobody is resting, which is what keeps this free when it is not
            // in use.
            None if !covered => return,
            _ => {}
        }
        let from = self.black(output, now).map_or(0.0, |(_, black, _)| black);
        let (id, commits) = match existing.map(|index| self.sheets.swap_remove(index)) {
            // The same element carrying on in the other direction.
            Some(sheet) => {
                let commits = commit_of(&sheet, now);
                (sheet.id, commits)
            }
            None => (Id::new(), 0),
        };
        self.sheets.push(Sheet {
            output: name,
            id,
            started: now,
            down: covered,
            from,
            commits,
        });
    }

    /// What to draw over `output` now: the element's id, how black it is, and
    /// the commit that says it has changed since the last frame. `None` when
    /// this display has nothing over it.
    pub fn black(&self, output: &Output, now: Instant) -> Option<(Id, f32, CommitCounter)> {
        let name = output.name();
        let sheet = self.sheets.iter().find(|sheet| sheet.output == name)?;
        let black = travelled(sheet.from, sheet.down, elapsed(sheet, now));
        if black <= 0.0 {
            return None;
        }
        Some((
            sheet.id.clone(),
            black,
            CommitCounter::from(commit_of(sheet, now)),
        ))
    }

    /// Whether `output` is all the way black and staying there: the sheet has
    /// finished coming down and nobody has asked for it back.
    ///
    /// The moment nothing on that display can be seen, which is when what is on
    /// it stops being on screen — see [`crate::render::windows_on_screen`]. Not
    /// a moment earlier, because a display on its way down is still showing its
    /// picture through a fading sheet; and not a moment later, because the
    /// instant a sheet is asked back up the user is waiting for what is under
    /// it, and it has to be running by the time it is uncovered.
    pub fn is_black(&self, output: &Output, now: Instant) -> bool {
        let name = output.name();
        // Read off the clock rather than off the alpha: every leg takes the
        // whole of its length however far it has to travel, so a sheet that
        // has been coming down for that long is black, including one that was
        // turned round halfway — whose alpha may land a rounding short of one.
        self.sheets
            .iter()
            .any(|sheet| sheet.output == name && sheet.down && elapsed(sheet, now) >= DOWN)
    }

    /// Drop the sheets that have finished coming back up, and the ones whose
    /// display has gone away. Called once a pass of the session's loop, beside
    /// the flashes that have burnt out.
    ///
    /// A display that is unplugged while resting takes its sheet with it: the
    /// one that comes back is a display the shell has not yet decided anything
    /// about, and bringing it back already black would be a screen that comes
    /// up dead.
    pub fn prune(&mut self, live: &[Output], now: Instant) {
        let names: Vec<String> = live.iter().map(Output::name).collect();
        self.sheets.retain(|sheet| {
            names.contains(&sheet.output)
                && travelled(sheet.from, sheet.down, elapsed(sheet, now)) > 0.0
        });
    }
}

/// How long this leg of the sheet's journey has been running.
fn elapsed(sheet: &Sheet, now: Instant) -> Duration {
    now.saturating_duration_since(sheet.started)
}

/// The commit this sheet reports at `now`: where it started this leg, plus the
/// milliseconds it has been moving. It stops moving when the fade does, which
/// is what makes a resting display free — nothing changes, no damage is
/// reported, no frame is queued, and the last black frame stays on the panel
/// for as long as the game lasts.
fn commit_of(sheet: &Sheet, now: Instant) -> usize {
    sheet.commits + elapsed(sheet, now).min(length(sheet.down)).as_millis() as usize
}

/// How long a move in this direction takes.
fn length(down: bool) -> Duration {
    if down {
        DOWN
    } else {
        UP
    }
}

/// How black the sheet is `since` into a move that started at `from` and is
/// going to black (`down`) or off it, smoothstepped so neither end is a jump.
///
/// The whole of that direction's length however far it has to travel, which is
/// the same bargain the curtain makes: a sheet turned round after a moment
/// comes back slower than it went, and the alternative — shortening the journey
/// to match — buys a reversal that snaps.
fn travelled(from: f32, down: bool, since: Duration) -> f32 {
    let to = if down { 1.0 } else { 0.0 };
    let t = smoothstep(since.as_secs_f32() / length(down).as_secs_f32());
    (from + (to - from) * t).clamp(0.0, 1.0)
}

fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whether this display is fully black — the way the tests read the sheet,
    /// since what the render pass wants is the alpha and not a verdict.
    fn is_black(black: &Blackouts, output: &Output, now: Instant) -> bool {
        black
            .black(output, now)
            .is_some_and(|(_, alpha, _)| alpha >= 1.0)
    }

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

    /// Nothing is drawn over a session where nobody is playing anything.
    #[test]
    fn a_display_nobody_is_resting_has_no_sheet() {
        let screen = output("A");
        let mut black = Blackouts::default();
        let now = Instant::now();
        assert!(black.black(&screen, now).is_none());
        // And asking to bring back a display that was never rested makes no
        // sheet to bring back.
        black.cover(&screen, false, now);
        assert!(black.black(&screen, now).is_none());
    }

    /// It goes black over [`DOWN`] and stays there until it is asked back.
    #[test]
    fn a_rested_display_goes_black_and_stays() {
        let screen = output("A");
        let mut black = Blackouts::default();
        let t0 = Instant::now();
        black.cover(&screen, true, t0);

        let (_, start, _) = black
            .black(&screen, t0 + Duration::from_millis(1))
            .expect("black from the first frame");
        assert!(start < 0.25, "the fade starts dark: {start}");
        assert_eq!(black.black(&screen, t0 + DOWN / 2).unwrap().1, 0.5);
        assert_eq!(black.black(&screen, t0 + DOWN).unwrap().1, 1.0);
        assert!(is_black(&black, &screen, t0 + DOWN * 20));
    }

    /// One display resting says nothing about the one beside it, which is the
    /// whole point of a sheet per display.
    #[test]
    fn resting_one_display_leaves_the_other_alone() {
        let (a, b) = (output("A"), output("B"));
        let mut black = Blackouts::default();
        let t0 = Instant::now();
        black.cover(&a, true, t0);
        assert!(is_black(&black, &a, t0 + DOWN));
        assert!(black.black(&b, t0 + DOWN).is_none());
    }

    /// Coming back is quicker than going, because by then somebody is waiting
    /// for the picture.
    #[test]
    fn a_display_comes_back_faster_than_it_went() {
        assert!(UP < DOWN);
        let screen = output("A");
        let mut black = Blackouts::default();
        let t0 = Instant::now();
        black.cover(&screen, true, t0);
        let landed = t0 + DOWN;
        black.cover(&screen, false, landed);
        assert_eq!(black.black(&screen, landed + UP / 2).unwrap().1, 0.5);
        assert!(black.black(&screen, landed + UP).is_none());
    }

    /// Turned round mid-fade it carries on from where it is, and it is the same
    /// element that carries on — a second element appearing over the first
    /// would be two blacks on one screen.
    #[test]
    fn a_sheet_asked_back_starts_from_where_it_got_to() {
        let screen = output("A");
        let mut black = Blackouts::default();
        let t0 = Instant::now();
        black.cover(&screen, true, t0);
        let (id, half, _) = black.black(&screen, t0 + DOWN / 2).unwrap();
        assert_eq!(half, 0.5);
        black.cover(&screen, false, t0 + DOWN / 2);
        let (same, still, _) = black.black(&screen, t0 + DOWN / 2).unwrap();
        assert_eq!(still, 0.5, "it did not jump to black on the way up");
        assert_eq!(same, id, "and it is the same element coming back");
        assert!(black.black(&screen, t0 + DOWN / 2 + UP).is_none());
    }

    /// The damage tracker has to see it move, and has to see it move *forward*:
    /// a screen rested and woken all evening would otherwise hand back numbers
    /// it has already been given.
    #[test]
    fn the_commit_moves_with_the_fade_and_only_forward() {
        let screen = output("A");
        let mut black = Blackouts::default();
        let t0 = Instant::now();
        black.cover(&screen, true, t0);
        let commit = |black: &Blackouts, at| black.black(&screen, at).unwrap().2;
        let early = commit(&black, t0 + Duration::from_millis(16));
        let later = commit(&black, t0 + Duration::from_millis(32));
        assert_ne!(early, later);
        // And stops with the fade, which is what makes a resting screen free.
        assert_eq!(commit(&black, t0 + DOWN), commit(&black, t0 + DOWN * 4));

        let landed = t0 + DOWN;
        black.cover(&screen, false, landed);
        assert!(
            commit(&black, landed + Duration::from_millis(16)).distance(Some(later)) > Some(0),
            "the way back carries on from where the way down stopped"
        );
    }

    /// All the way black is the end of the way down and nothing else: not the
    /// fade on its way there, and not a sheet that has been asked back up, even
    /// on the very frame it was asked — what is under it has to be running by
    /// the time it can be seen.
    #[test]
    fn a_display_is_black_only_once_it_is_all_the_way_down() {
        let (a, b) = (output("A"), output("B"));
        let mut black = Blackouts::default();
        let t0 = Instant::now();
        assert!(!black.is_black(&a, t0), "a display nobody rested");

        black.cover(&a, true, t0);
        assert!(!black.is_black(&a, t0 + DOWN / 2), "still fading down");
        assert!(black.is_black(&a, t0 + DOWN));
        assert!(black.is_black(&a, t0 + DOWN * 20));
        assert!(!black.is_black(&b, t0 + DOWN), "and only that display");

        let woken = t0 + DOWN * 20;
        black.cover(&a, false, woken);
        assert!(!black.is_black(&a, woken), "asked back up is not black");
        assert!(!black.is_black(&a, woken + UP / 2));
    }

    /// Asking again for what is already happening does not restart it, which is
    /// what lets the shell say what it wants once a frame.
    #[test]
    fn asking_twice_does_not_put_the_fade_back_to_the_start() {
        let screen = output("A");
        let mut black = Blackouts::default();
        let t0 = Instant::now();
        black.cover(&screen, true, t0);
        black.cover(&screen, true, t0 + DOWN / 2);
        assert_eq!(black.black(&screen, t0 + DOWN).unwrap().1, 1.0);
    }

    /// A sheet that has finished coming up is dropped, and so is one whose
    /// display has been unplugged — a screen must never come back already dead.
    #[test]
    fn finished_and_departed_sheets_are_dropped() {
        let (a, b) = (output("A"), output("B"));
        let mut black = Blackouts::default();
        let t0 = Instant::now();
        black.cover(&a, true, t0);
        black.cover(&b, true, t0);

        black.prune(&[a.clone(), b.clone()], t0 + DOWN);
        assert!(is_black(&black, &a, t0 + DOWN) && is_black(&black, &b, t0 + DOWN));

        // B is unplugged while resting.
        black.prune(std::slice::from_ref(&a), t0 + DOWN);
        assert!(black.black(&b, t0 + DOWN).is_none());

        // And A is brought back and finishes coming back.
        black.cover(&a, false, t0 + DOWN);
        black.prune(std::slice::from_ref(&a), t0 + DOWN + UP);
        assert!(black.black(&a, t0 + DOWN + UP).is_none());
    }
}
