//! The little that is still shown on a display that has gone dark.
//!
//! A screen the session has taken down to black — rested behind
//! [`crate::blackout`]'s sheet while somebody uses another one, or switched off
//! by the idle timer because nobody is using the machine at all — is a screen
//! with nothing on it. That is the point of it, and on an OLED panel it is the
//! whole point: black pixels are unlit pixels, and an unlit pixel cannot be
//! burnt.
//!
//! But a dark screen in a room is still a screen somebody glances at, and the
//! thing they glance at it for is the time. So the shell may ask for one
//! surface of its own to be drawn over that black, and this is the register of
//! which surface that is, per display.
//!
//! ## What this module is, and is not
//!
//! It is a *name*, nothing more: `lxb_shell_v1.set_always_on_display` hands
//! over a `wl_surface` and this remembers it against the display it belongs to.
//! The drawing is the shell's — the compositor has no letters to write a clock
//! with — and the lifting is [`crate::render`]'s, which puts the named surface
//! over the sheet instead of under it.
//!
//! It is not a second black, and it does not make the display any less dark.
//! [`crate::blackout::Blackouts::is_black`] goes on saying yes for a display
//! showing one of these, which is what keeps that display's windows out of
//! [`crate::render::windows_on_screen`] and their applications stopped by
//! [`crate::sleep`]. The only thing that changes about such a display is that
//! one surface of the shell's is drawn, and that its connector is left on.
//!
//! ## The connector
//!
//! That last part is the cost, and it is worth saying plainly. A display the
//! shell switches off is normally taken down at the connector once its black is
//! all the way down — on a panel the backlight going out, on a monitor the
//! monitor going to sleep — and that is where the power saving in Settings >
//! Power actually comes from. A display showing one of these is left lit
//! instead, because a connector that is off has nothing to show a clock on. So
//! a screen with this turned on costs what a dark-but-lit panel costs, which on
//! an OLED showing a handful of near-black pixels is very little and on an LCD
//! is the backlight. See [`Always::showing`], which is the one question the
//! backend asks.

use smithay::output::Output;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::IsAlive;

/// Which surface carries each display's always-on display.
///
/// Keyed by connector name rather than by [`Output`], like everything else kept
/// per display here: it is the name the shell files its own settings under, and
/// it survives a display being unplugged and plugged back in. A name with no
/// entry is a display showing nothing, which is every display until somebody
/// turns the setting on.
#[derive(Debug, Default)]
pub struct Always {
    named: std::collections::HashMap<String, WlSurface>,
}

impl Always {
    /// Name the surface one display's clock is drawn on, or forget it.
    ///
    /// `true` when that is a change, which is what the caller redraws on: a
    /// shell that names the same surface twice has not asked for anything to
    /// happen.
    pub fn name(&mut self, output: &Output, surface: Option<WlSurface>) -> bool {
        let name = output.name();
        match surface {
            Some(surface) => self.named.insert(name, surface.clone()) != Some(surface),
            None => self.named.remove(&name).is_some(),
        }
    }

    /// The surface this display's clock is drawn on, if one is named and still
    /// alive.
    ///
    /// A surface the client has destroyed is not one anybody can draw, and it
    /// is forgotten rather than reported: the shell is expected to send null
    /// before it goes, but a shell that died holding one must not leave a
    /// display believing it still has a clock to show — that display would keep
    /// its connector lit for ever.
    pub fn surface(&self, output: &Output) -> Option<&WlSurface> {
        self.named
            .get(&output.name())
            .filter(|surface| surface.alive())
    }

    /// Whether this display is showing an always-on display.
    ///
    /// The one question the backend asks, and the reason it asks it: a display
    /// answering yes keeps its connector on through a switch-off it would
    /// otherwise be taken down by. See the module's own note about what that
    /// costs.
    pub fn showing(&self, output: &Output) -> bool {
        self.surface(output).is_some()
    }

    /// Forget a surface that has gone, whoever it belonged to.
    ///
    /// Called when a client destroys a surface, so that a display does not go
    /// on being held lit by a name that no longer answers.
    pub fn forget_surface(&mut self, surface: &WlSurface) {
        self.named.retain(|_, named| named != surface);
    }

    /// Forget a display that has gone.
    pub fn forget(&mut self, output: &Output) {
        self.named.remove(&output.name());
    }
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

    /// A display nobody has named a surface for shows nothing, which is every
    /// display until the setting is turned on.
    #[test]
    fn a_display_nobody_has_named_shows_nothing() {
        let always = Always::default();
        assert!(!always.showing(&output("one")));
    }

    /// Forgetting a display nobody named is not a change, so a shell that sends
    /// null on every pass does not make the compositor redraw on every pass.
    #[test]
    fn forgetting_what_was_never_named_changes_nothing() {
        let mut always = Always::default();
        assert!(!always.name(&output("one"), None));
    }

    /// A display going away takes its name with it, and leaves every other
    /// display's alone. A screen unplugged while it was showing a clock must
    /// not hold a connector lit that nothing is driving any more.
    #[test]
    fn a_display_that_goes_away_leaves_the_others_alone() {
        let mut always = Always::default();
        always.forget(&output("one"));
        assert!(!always.showing(&output("two")));
    }
}
