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
//! ## The same sheet, for a machine nobody is using
//!
//! The shell's idle timers use this sheet as well — see
//! `lxb_shell_v1.set_output_power` and [`Power`]. **Dim** is the sheet stopped
//! part of the way down, which leaves a screen plainly waiting; **off** is the
//! sheet all the way down and then the connector switched off behind it, which
//! is the one case here that *is* a display being switched off, and it is the
//! backend that does that part — see [`Blackouts::is_off`]. Both are asked for
//! by the shell and neither is undone by input on its own: the shell hears
//! about activity over ext-idle-notify, and a controller the compositor never
//! sees counts too.
//!
//! One sheet per display, whatever asked for it. A screen resting behind OLED
//! protection that the idle timer then dims is a screen that is already black,
//! and two sheets would be two blacks and two fades to reason about. The sheet
//! heads for the darkest thing anybody wants of that display, and when one
//! reason lets go it heads for whatever the other still wants.
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
pub(crate) const DOWN: Duration = Duration::from_millis(1000);

/// And how long it takes to come back.
///
/// A quarter of a second, because by the time this runs the user has already
/// asked for the screen — they have moved the pointer onto it, or taken the
/// display over — and everything after the asking is delay. It is short rather
/// than instant only so that the picture arrives rather than appears.
const UP: Duration = Duration::from_millis(250);

/// How much of a display's light a dimmed display keeps back.
///
/// A little over half. What dimming is for is the corner of an eye: somebody
/// who has looked away from a screen for a minute should look back at one that
/// is plainly waiting, and still be able to read what it is waiting on. Much
/// darker than this reads as switched off, which is the next step and has its
/// own sheet; much lighter reads as the picture having changed.
pub const DIM: f32 = 0.55;

/// How lit the shell wants one display kept while nobody is using the machine.
///
/// `lxb_shell_v1.display_power`, as this compositor holds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Power {
    /// Drawn as it always is.
    #[default]
    On,
    /// Under a sheet that takes [`DIM`] of its light away.
    Dim,
    /// Faded to black, and then switched off at the connector by the backend.
    Off,
}

impl Power {
    /// How far down the sheet has to be for this.
    fn level(self) -> f32 {
        match self {
            Power::On => 0.0,
            Power::Dim => DIM,
            Power::Off => 1.0,
        }
    }
}

/// One display's sheet, and how far it has got.
#[derive(Debug)]
struct Sheet {
    /// By name, because a display can go away and come back underneath a fade,
    /// and a name compares equal to the display that returns.
    output: String,
    /// Stable for the life of the sheet, so the damage tracker sees one element
    /// changing rather than a new element every frame.
    id: Id,
    /// When it started moving, and where to: 0 is clear, 1 is black, and
    /// anything between is a sheet stopped part of the way — which is what a
    /// dimmed display is.
    started: Instant,
    to: f32,
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
    /// The displays OLED protection has rested, by name.
    rested: std::collections::HashSet<String>,
    /// The displays the shell has dimmed or switched off, by name. A display
    /// that is on is not in here.
    power: std::collections::HashMap<String, Power>,
}

impl Blackouts {
    /// Take `output` down to black, or bring it back, from `now` — OLED
    /// protection's half of the sheet.
    ///
    /// Asking for what is already happening is ignored rather than restarted,
    /// so a shell that repeats itself does not put the fade back to the
    /// beginning — and neither does one that says so once a frame, which is how
    /// a value the shell holds is ordinarily kept in step.
    pub fn cover(&mut self, output: &Output, covered: bool, now: Instant) {
        let name = output.name();
        if covered {
            self.rested.insert(name);
        } else {
            self.rested.remove(&name);
        }
        self.aim(output, now);
    }

    /// Dim `output`, switch it off, or bring it back, from `now` — the idle
    /// timer's half of the sheet. See [`Power`].
    pub fn set_power(&mut self, output: &Output, power: Power, now: Instant) {
        let name = output.name();
        if power == Power::On {
            self.power.remove(&name);
        } else {
            self.power.insert(name, power);
        }
        self.aim(output, now);
    }

    /// What the shell has asked of `output`'s light.
    pub fn power(&self, output: &Output) -> Power {
        self.power.get(&output.name()).copied().unwrap_or_default()
    }

    /// Whether any display is dimmed or switched off.
    pub fn any_powered_down(&self) -> bool {
        !self.power.is_empty()
    }

    /// Start the sheet towards the darkest thing anybody wants of `output`, if
    /// it is not already on its way there.
    fn aim(&mut self, output: &Output, now: Instant) {
        let name = output.name();
        let rested = if self.rested.contains(&name) {
            1.0
        } else {
            0.0
        };
        let to = f32::max(rested, self.power(output).level());
        let existing = self.sheets.iter().position(|sheet| sheet.output == name);
        match existing {
            Some(index) if self.sheets[index].to == to => return,
            // Nothing to bring back. There is no sheet at all for a display
            // nobody is resting, which is what keeps this free when it is not
            // in use.
            None if to <= 0.0 => return,
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
            to,
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
        let black = travelled(sheet.from, sheet.to, elapsed(sheet, now));
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
            .any(|sheet| sheet.output == name && sheet.to >= 1.0 && elapsed(sheet, now) >= DOWN)
    }

    /// Whether `output` has been switched off and its sheet has finished coming
    /// down: the moment the backend turns its connector off. A display on its
    /// way down is still showing its picture through the fade, and one being
    /// brought back has to be lit before it can be seen coming back.
    pub fn is_off(&self, output: &Output, now: Instant) -> bool {
        self.power(output) == Power::Off && self.is_black(output, now)
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
                && (sheet.to > 0.0 || travelled(sheet.from, sheet.to, elapsed(sheet, now)) > 0.0)
        });
        // And what was asked of a display that has gone, for the same reason:
        // the one that comes back has not been asked anything yet.
        self.rested.retain(|name| names.contains(name));
        self.power.retain(|name, _| names.contains(name));
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
    sheet.commits
        + elapsed(sheet, now)
            .min(length(sheet.from, sheet.to))
            .as_millis() as usize
}

/// How long a move from `from` to `to` takes: [`DOWN`] for any move darker,
/// dimming included, and [`UP`] for any move lighter.
fn length(from: f32, to: f32) -> Duration {
    if to > from {
        DOWN
    } else {
        UP
    }
}

/// How black the sheet is `since` into a move that started at `from` and is
/// going to `to`, smoothstepped so neither end is a jump.
///
/// The whole of that direction's length however far it has to travel, which is
/// the same bargain the curtain makes: a sheet turned round after a moment
/// comes back slower than it went, and the alternative — shortening the journey
/// to match — buys a reversal that snaps.
fn travelled(from: f32, to: f32, since: Duration) -> f32 {
    let t = smoothstep(since.as_secs_f32() / length(from, to).as_secs_f32());
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

    /// Dimming stops the sheet part of the way down and leaves it there, and
    /// a dimmed display is not black: what is on it is still on screen.
    #[test]
    fn a_dimmed_display_stops_part_of_the_way_down() {
        let screen = output("A");
        let mut black = Blackouts::default();
        let t0 = Instant::now();
        black.set_power(&screen, Power::Dim, t0);
        assert_eq!(black.black(&screen, t0 + DOWN).unwrap().1, DIM);
        assert_eq!(black.black(&screen, t0 + DOWN * 30).unwrap().1, DIM);
        assert!(!black.is_black(&screen, t0 + DOWN * 30));
        assert!(!black.is_off(&screen, t0 + DOWN * 30));
        // And pruning leaves a dimmed display dimmed, however long it waits.
        black.prune(std::slice::from_ref(&screen), t0 + DOWN * 30);
        assert_eq!(black.black(&screen, t0 + DOWN * 31).unwrap().1, DIM);
    }

    /// Off carries on from the dim to black, and is off only once it is all the
    /// way there — the backend switches the connector off at that moment and
    /// not before.
    #[test]
    fn a_display_is_off_once_it_is_black_and_not_before() {
        let screen = output("A");
        let mut black = Blackouts::default();
        let t0 = Instant::now();
        black.set_power(&screen, Power::Dim, t0);
        let dimmed = t0 + DOWN;
        black.set_power(&screen, Power::Off, dimmed);
        let (_, from, _) = black.black(&screen, dimmed).unwrap();
        assert_eq!(from, DIM, "it carries on from the dim");
        assert!(!black.is_off(&screen, dimmed + DOWN / 2));
        assert!(black.is_off(&screen, dimmed + DOWN));
        assert!(black.is_black(&screen, dimmed + DOWN));

        let woken = dimmed + DOWN * 10;
        black.set_power(&screen, Power::On, woken);
        assert!(!black.is_off(&screen, woken), "on is on at once");
        assert!(black.black(&screen, woken + UP).is_none());
        assert_eq!(black.power(&screen), Power::On);
    }

    /// A display resting behind OLED protection that the idle timer also dims
    /// stays black, and bringing it back from the idle leaves it resting.
    #[test]
    fn the_sheet_heads_for_the_darkest_thing_anybody_wants() {
        let screen = output("A");
        let mut black = Blackouts::default();
        let t0 = Instant::now();
        black.cover(&screen, true, t0);
        black.set_power(&screen, Power::Dim, t0 + DOWN);
        assert!(black.is_black(&screen, t0 + DOWN * 2), "still resting");
        black.set_power(&screen, Power::On, t0 + DOWN * 2);
        assert!(black.is_black(&screen, t0 + DOWN * 3), "and still resting");
        // Let go of the rest while dimmed, and it comes up to the dim.
        black.set_power(&screen, Power::Dim, t0 + DOWN * 3);
        black.cover(&screen, false, t0 + DOWN * 4);
        assert_eq!(black.black(&screen, t0 + DOWN * 4 + UP).unwrap().1, DIM);
    }

    /// An unplugged display forgets what it was asked, so the one that comes
    /// back is not born dimmed or dark.
    #[test]
    fn a_departed_display_forgets_its_power() {
        let (a, b) = (output("A"), output("B"));
        let mut black = Blackouts::default();
        let t0 = Instant::now();
        black.set_power(&b, Power::Off, t0);
        assert!(black.any_powered_down());
        black.prune(std::slice::from_ref(&a), t0 + DOWN);
        assert_eq!(black.power(&b), Power::On);
        assert!(!black.any_powered_down());
    }
}
