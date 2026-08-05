//! Multi-display layout.
//!
//! Gamescope is single-output by construction: it owns one CRTC and scales one
//! application onto it. Linboard keeps the same "one app fills the screen"
//! model but tracks an arbitrary number of outputs, each with its own logical
//! position, scale, transform and window stack.
//!
//! Outputs are laid out in a stable order (the order they were first seen), so
//! unplugging and replugging a monitor does not shuffle the others around.

use smithay::desktop::{layer_map_for_output, Space, Window};
use smithay::output::{Output, Scale};
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::utils::{Logical, Point, Rectangle, Size, Transform};
use smithay::wayland::shell::xdg::ToplevelStateSet;

use crate::config::{Config, OutputLayout};
use crate::input::window_is_x11_chrome;

/// Move a mapped window without changing its position in the compositor's
/// stack. `Space::map_element` always raises existing elements, which is not
/// desirable for routine configure notifications and output relayouts.
pub(crate) fn remap_window_preserving_stack(
    space: &mut Space<Window>,
    window: &Window,
    location: Point<i32, Logical>,
) {
    let order: Vec<Window> = space.elements().cloned().collect();
    let was_mapped = order.iter().any(|element| element == window);

    space.map_element(window.clone(), location, false);
    if was_mapped {
        for element in order {
            space.raise_element(&element, false);
        }
    }
}

/// The states every application window is configured with.
///
/// `Maximized` is the load-bearing one: it is what turns the size in the
/// configure from a suggestion into an instruction. The tiled edges say the
/// same thing to clients that reason about their surroundings rather than
/// their state, and they are also what tells a client drawing its own
/// decorations to square off its corners and drop its shadow — there is no
/// desktop behind this window for a shadow to fall on.
pub(crate) fn set_maximized_states(states: &mut ToplevelStateSet) {
    states.set(xdg_toplevel::State::Maximized);
    states.set(xdg_toplevel::State::TiledLeft);
    states.set(xdg_toplevel::State::TiledRight);
    states.set(xdg_toplevel::State::TiledTop);
    states.set(xdg_toplevel::State::TiledBottom);
}

/// Tracks the logical arrangement of every enabled output.
#[derive(Debug, Default)]
pub struct OutputManager {
    /// Outputs in the order they were added; drives auto-placement.
    order: Vec<Output>,
}

impl OutputManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// Register a new output and place it according to `config`.
    pub fn add_output(&mut self, space: &mut Space<Window>, output: &Output, config: &Config) {
        if self.order.iter().any(|o| o == output) {
            return;
        }
        self.order.push(output.clone());
        self.relayout(space, config);
    }

    /// Drop an output (hotplug removal) and re-pack the survivors.
    pub fn remove_output(&mut self, space: &mut Space<Window>, output: &Output, config: &Config) {
        self.order.retain(|o| o != output);
        space.unmap_output(output);
        self.relayout(space, config);
    }

    /// Recompute logical positions for every output, then re-tile windows.
    ///
    /// Outputs with an explicit `position` in the config are pinned there and
    /// take no part in auto-placement; the rest are packed in order.
    pub fn relayout(&mut self, space: &mut Space<Window>, config: &Config) {
        let layout = config.general.output_layout;
        let gap = config.general.output_gap;
        let mut cursor = 0i32;

        for output in &self.order {
            let name = output.name();
            let entry = config.output_for(&name);

            let size = logical_size(output);

            let position = match entry.and_then(|e| e.position) {
                Some([x, y]) => Point::from((x, y)),
                None => match layout {
                    OutputLayout::Horizontal => {
                        let p = Point::from((cursor, 0));
                        cursor += size.w + gap;
                        p
                    }
                    OutputLayout::Vertical => {
                        let p = Point::from((0, cursor));
                        cursor += size.h + gap;
                        p
                    }
                    OutputLayout::Mirror => Point::from((0, 0)),
                },
            };

            space.map_output(output, position);
            // `map_output` only records the position inside the space; the
            // output's own location is what wl_output and xdg_output advertise,
            // so it has to be set too or every client sees all displays stacked
            // at the origin.
            output.change_current_state(None, None, None, Some(position));

            tracing::debug!(
                output = %name,
                x = position.x,
                y = position.y,
                w = size.w,
                h = size.h,
                "placed output"
            );
        }

        self.relayout_windows(space);
    }

    /// Apply the config's scale/transform/mode preferences to a freshly created
    /// output. The caller is responsible for the mode itself, since only the
    /// backend knows which modes the hardware actually offers.
    pub fn apply_output_config(output: &Output, config: &Config) {
        let name = output.name();
        let Some(entry) = config.output_for(&name) else {
            return;
        };

        let scale = entry.scale.map(Scale::Fractional);
        let transform = entry.transform.as_deref().and_then(parse_transform);

        if scale.is_some() || transform.is_some() {
            output.change_current_state(None, transform, scale, None);
        }
    }

    /// Whether the config disables this connector outright.
    pub fn is_enabled(name: &str, config: &Config) -> bool {
        config
            .output_for(name)
            .and_then(|e| e.enabled)
            .unwrap_or(true)
    }

    /// The output under a logical point, falling back to the first output.
    pub fn output_at(&self, space: &Space<Window>, point: Point<f64, Logical>) -> Option<Output> {
        space
            .output_under(point)
            .next()
            .cloned()
            .or_else(|| self.order.first().cloned())
    }

    /// The usable area of an output: its geometry minus layer-shell exclusive
    /// zones (panels, docks, and the XMB shell when it reserves space).
    pub fn usable_area(space: &Space<Window>, output: &Output) -> Option<Rectangle<i32, Logical>> {
        let geometry = space.output_geometry(output)?;
        let mut zone = layer_map_for_output(output).non_exclusive_zone();
        // `non_exclusive_zone` is output-relative; lift it into space coordinates.
        zone.loc += geometry.loc;
        Some(zone)
    }

    /// Size a single window to fill its output's usable area.
    pub fn tile_window(&self, space: &mut Space<Window>, window: &Window) {
        let output = space
            .outputs_for_element(window)
            .first()
            .cloned()
            .or_else(|| space.outputs().next().cloned());
        if let Some(output) = output {
            self.tile_window_on_output(space, window, &output);
        }
    }

    /// Size a window on an explicit output. Newly mapped/moved elements do
    /// not have refreshed Space output associations yet, so callers that
    /// already chose a target must not infer it through
    /// `outputs_for_element`.
    pub fn tile_window_on_output(
        &self,
        space: &mut Space<Window>,
        window: &Window,
        output: &Output,
    ) {
        // X11 chrome (override-redirect popups as well as managed menus,
        // notifications and splash windows) chooses its own geometry and must
        // never become a full-output application window during relayout.
        if window_is_x11_chrome(window) {
            return;
        }
        if window
            .x11_surface()
            .is_some_and(|surface| surface.is_override_redirect())
        {
            return;
        }

        let Some(area) = Self::usable_area(space, output) else {
            return;
        };

        if let Some(toplevel) = window.toplevel() {
            toplevel.with_pending_state(|state| {
                state.size = Some(area.size);
                // A size alone is only advisory. xdg-shell lets a client pick
                // its own dimensions unless the configure also carries a state
                // that makes the size binding, so without this a client maps at
                // whatever size it likes — typically its remembered desktop
                // geometry, which then hangs off the edge of the output and
                // only snaps into place once the user maximizes it by hand.
                set_maximized_states(&mut state.states);
                // For the window's own idea of a sensible size, before and
                // outside of any state we impose.
                state.bounds = Some(area.size);
            });
            toplevel.send_pending_configure();
        } else if let Some(surface) = window.x11_surface() {
            // The X11 equivalent: _NET_WM_STATE_MAXIMIZED_{HORZ,VERT}, so a
            // toolkit that sizes itself from the state property agrees with the
            // geometry we are about to hand it.
            if let Err(err) = surface.set_maximized(true) {
                tracing::warn!(?err, "failed to mark X11 window maximized");
            }
            if let Err(err) = surface.configure(area) {
                tracing::warn!(?err, "failed to tile X11 window");
            }
        }
        remap_window_preserving_stack(space, window, area.loc);
    }

    /// Re-tile every mapped window. Cheap enough to call on any layout change.
    pub fn relayout_windows(&self, space: &mut Space<Window>) {
        let windows: Vec<Window> = space.elements().cloned().collect();
        for window in windows {
            self.tile_window(space, &window);
        }
    }
}

/// The output's size in logical coordinates, honouring scale and transform.
///
/// This must round exactly as `Space::output_geometry` does — it ceils — or at
/// a fractional scale the layout would advance the cursor by a width that
/// disagrees with the geometry every hit-test and render path reads back,
/// leaving neighbouring outputs a pixel apart or a pixel overlapped.
fn logical_size(output: &Output) -> Size<i32, Logical> {
    let Some(mode) = output.current_mode() else {
        return Size::from((0, 0));
    };
    output
        .current_transform()
        .transform_size(mode.size)
        .to_f64()
        .to_logical(output.current_scale().fractional_scale())
        .to_i32_ceil()
}

fn parse_transform(raw: &str) -> Option<Transform> {
    Some(match raw.trim().to_ascii_lowercase().as_str() {
        "normal" | "0" => Transform::Normal,
        "90" => Transform::_90,
        "180" => Transform::_180,
        "270" => Transform::_270,
        "flipped" => Transform::Flipped,
        "flipped-90" | "flipped90" => Transform::Flipped90,
        "flipped-180" | "flipped180" => Transform::Flipped180,
        "flipped-270" | "flipped270" => Transform::Flipped270,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point of the state set: a configure that carries a size but
    /// no `Maximized` leaves the size advisory, and a client is free to map at
    /// its own remembered geometry — half of it hanging off the output until
    /// the user maximizes it by hand.
    #[test]
    fn a_tiled_window_is_told_its_size_is_binding() {
        let mut states = ToplevelStateSet::default();
        set_maximized_states(&mut states);
        assert!(states.contains(xdg_toplevel::State::Maximized));
    }

    #[test]
    fn a_tiled_window_is_tiled_on_every_edge() {
        let mut states = ToplevelStateSet::default();
        set_maximized_states(&mut states);
        for edge in [
            xdg_toplevel::State::TiledLeft,
            xdg_toplevel::State::TiledRight,
            xdg_toplevel::State::TiledTop,
            xdg_toplevel::State::TiledBottom,
        ] {
            assert!(states.contains(edge), "{edge:?} missing");
        }
    }

    /// Re-tiling a window that is already maximized must not look like a
    /// change, or every relayout would send a redundant configure.
    #[test]
    fn setting_the_states_twice_changes_nothing() {
        let mut once = ToplevelStateSet::default();
        set_maximized_states(&mut once);
        let mut twice = once.clone();
        set_maximized_states(&mut twice);
        assert_eq!(once, twice);
    }

    #[test]
    fn parses_transforms() {
        assert_eq!(parse_transform("normal"), Some(Transform::Normal));
        assert_eq!(parse_transform("90"), Some(Transform::_90));
        assert_eq!(parse_transform("flipped-180"), Some(Transform::Flipped180));
        assert_eq!(parse_transform("sideways"), None);
    }
}
