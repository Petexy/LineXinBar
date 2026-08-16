//! Render element assembly, shared by every backend.
//!
//! Both the nested and the DRM backend need the exact same element list for a
//! given output, so it is built once here and is generic over the renderer.

use smithay::backend::renderer::element::memory::MemoryRenderBufferRenderElement;
use smithay::backend::renderer::element::solid::SolidColorRenderElement;
use smithay::backend::renderer::element::surface::WaylandSurfaceRenderElement;
use smithay::backend::renderer::element::utils::RescaleRenderElement;
use smithay::backend::renderer::element::{AsRenderElements, Kind};
use smithay::backend::renderer::utils::CommitCounter;
use smithay::backend::renderer::{ImportAll, ImportMem, Renderer};
use smithay::desktop::utils::OutputPresentationFeedback;
use smithay::desktop::{layer_map_for_output, Window};
use smithay::output::Output;
use smithay::utils::{Logical, Rectangle, Scale};
use smithay::wayland::shell::wlr_layer::Layer;

use crate::input::window_accepts_keyboard_focus;
use crate::state::Lxb;

pub use crate::cursor::CursorState;

smithay::render_elements! {
    /// Everything LineXinBar can put on screen.
    pub LxbRenderElement<R> where R: ImportAll + ImportMem;
    /// A client surface (window, layer surface, popup or cursor surface).
    Surface = WaylandSurfaceRenderElement<R>,
    /// A window mid-flight in the overview, drawn scaled into its card.
    OverviewCard = RescaleRenderElement<WaylandSurfaceRenderElement<R>>,
    /// A CPU-side image: the themed cursor.
    Memory = MemoryRenderBufferRenderElement<R>,
    /// One flat colour over the whole display: the flash a screenshot answers
    /// with, and nothing else so far.
    Solid = SolidColorRenderElement,
}

/// The windows the overview shows for `output`, topmost first — the same
/// order they are announced to the shell in, so slot N here is the card the
/// shell is framing as window N.
///
/// Whatever the shell is keeping out of sight is left out, which is what makes
/// one filter enough for both halves of the guide: a window that is not in
/// this list is not drawn a card, and is not announced as being on the display
/// either, so there is nothing there to select or to offer to close.
pub fn overview_windows(lxb: &Lxb, output: &Output) -> Vec<Window> {
    lxb.space
        .elements_for_output(output)
        .rev()
        .filter(|window| window_accepts_keyboard_focus(window) && !lxb.out_of_sight(window))
        .cloned()
        .collect()
}

/// Assemble the full element list for `output`, front to back.
///
/// Ordering follows wlr-layer-shell: overlay on top, then top, then regular
/// windows, then bottom and background. The cursor, when drawn by us, sits
/// above everything.
pub fn output_elements<R>(
    renderer: &mut R,
    lxb: &Lxb,
    output: &Output,
    cursor: Option<&mut CursorState>,
) -> Vec<LxbRenderElement<R>>
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Send + Clone + 'static,
{
    let space = &lxb.space;
    let now = std::time::Instant::now();
    let scale = Scale::from(output.current_scale().fractional_scale());
    let Some(output_geo) = space.output_geometry(output) else {
        return Vec::new();
    };

    let mut elements: Vec<LxbRenderElement<R>> = Vec::new();

    // The flash before everything, cursor included: a camera's answer is over
    // the whole screen or it is not an answer. It is never in the photograph —
    // the picture is read back before the flash is started — and it is only
    // ever here for the few frames after one was actually written.
    if let Some((id, white)) = lxb.flashes.white(output, now) {
        elements.push(LxbRenderElement::Solid(SolidColorRenderElement::new(
            id,
            Rectangle::from_size(output_geo.size.to_physical_precise_round(scale)),
            CommitCounter::default(),
            // Premultiplied, which is what the renderer draws: white at this
            // alpha is that alpha in all four channels.
            [white, white, white, white],
            Kind::Unspecified,
        )));
    }

    // Cursor first: elements are drawn front to back — and only while there is
    // one to draw. A pointer nobody has moved is left off the screen entirely
    // rather than drawn somewhere the user is not looking; see the doc comment
    // on `Lxb::pointer_visible`.
    let cursor = cursor.filter(|_| lxb.pointer_visible);
    if let Some(cursor) = cursor {
        let position = (lxb.pointer_location - output_geo.loc.to_f64())
            .to_physical(scale)
            .to_i32_round();
        elements.extend(cursor.render(renderer, position, scale, lxb.start_time.elapsed()));
    }

    // Everything from here on is the session itself. A frame that adds nothing
    // past this mark is a frame with no session in it yet, which is what the
    // startup wallpaper at the bottom of this function answers.
    let session_content_starts_at = elements.len();

    let layer_map = layer_map_for_output(output);

    let push_layer = |elements: &mut Vec<LxbRenderElement<R>>, layer: Layer, renderer: &mut R| {
        for surface in layer_map.layers_on(layer).rev() {
            let Some(geometry) = layer_map.layer_geometry(surface) else {
                continue;
            };
            let location = geometry.loc.to_physical_precise_round(scale);
            elements.extend(
                surface
                    .render_elements::<WaylandSurfaceRenderElement<R>>(
                        renderer, location, scale, 1.0,
                    )
                    .into_iter()
                    .map(LxbRenderElement::Surface),
            );
        }
    };

    // A window being brought back flies in front of everything, the shell's
    // own surface included. That is the point of it: the start screen it grows
    // out of has to still be there behind it, exactly as a phone's home screen
    // stays put while an application already in memory opens over it. Drawn
    // before the overlay rather than with the other windows, because the shell
    // is holding the overlay while this happens.
    let flying = push_restoring_windows(&mut elements, renderer, lxb, output, now, scale);

    push_layer(&mut elements, Layer::Overlay, renderer);
    push_layer(&mut elements, Layer::Top, renderer);

    // Windows, topmost first — either where they really are, or (in the
    // overview) somewhere between there and their card.
    let overview = lxb.overview.progress(output, now);
    if overview > 0.0 {
        push_overview_windows(&mut elements, renderer, lxb, output, overview, scale);
    } else {
        for window in space.elements_for_output(output).rev() {
            // Already drawn, mid-flight, in front of the shell.
            if flying.contains(&crate::overview::window_id(window)) {
                continue;
            }
            // An application the shell runs without showing. It is mapped,
            // configured and drawing exactly as it would be; the pixels simply
            // never leave it. See `lxb_shell_v1.keep_out_of_sight`.
            if lxb.out_of_sight(window) {
                continue;
            }
            let Some(location) = space.element_location(window) else {
                continue;
            };
            // A window is mapped by its *geometry* — the frame the user thinks
            // of as the window. A client drawing its own decorations puts that
            // frame inside a larger surface, with the drop shadow spilling
            // above and to the left of it, so the surface starts before the
            // geometry does. Rendering from the geometry's location instead
            // would push the whole window down and right by the width of its
            // own shadow, hanging its far edge off the display — and leave the
            // pixels disagreeing with input, which does subtract this.
            let relative = (location - output_geo.loc - window.geometry().loc)
                .to_physical_precise_round(scale);
            elements.extend(
                window
                    .render_elements::<WaylandSurfaceRenderElement<R>>(
                        renderer, relative, scale, 1.0,
                    )
                    .into_iter()
                    .map(LxbRenderElement::Surface),
            );
        }
    }

    push_layer(&mut elements, Layer::Bottom, renderer);
    push_layer(&mut elements, Layer::Background, renderer);

    // Behind everything, and only while there is no everything: the wallpaper
    // the session draws for itself, so the displays are never handed over to a
    // black screen — in either direction. See `crate::backdrop`.
    //
    // Decided from the frame rather than from the session's state, because
    // this is the question the frame is actually asking. A shell that has
    // started, been configured and not yet attached a buffer contributes no
    // element here; so does a shell that has exited and taken its surfaces
    // with it. Both are frames with no session in them, and both used to be
    // the clear colour.
    //
    // Asking for the element is also what keeps the wallpaper being painted:
    // this is the only place that knows it is on screen.
    if let Some(backdrop) = (elements.len() == session_content_starts_at)
        .then_some(lxb.backdrop.as_ref())
        .flatten()
    {
        match backdrop.element(renderer, output_geo.size) {
            Ok(element) => elements.push(LxbRenderElement::Memory(element)),
            // Nothing else in the frame depends on it, and the clear colour is
            // still underneath. A session that comes up is worth more than one
            // that refused to over its own wallpaper.
            Err(err) => tracing::warn!(?err, "could not upload the startup wallpaper"),
        }
    }

    elements
}

/// Draw every window mid-flight back out of the tile it was asked for on, and
/// return which ones were drawn so the pass below does not draw them twice.
///
/// The same interpolation the overview uses, read the other way round: the
/// window starts fitted into the rectangle the shell named and ends at its own
/// geometry. Both endpoints share its aspect ratio — `fit` is what guarantees
/// that — so the corners can be interpolated and the scale stays uniform.
fn push_restoring_windows<R>(
    elements: &mut Vec<LxbRenderElement<R>>,
    renderer: &mut R,
    lxb: &Lxb,
    output: &Output,
    now: std::time::Instant,
    scale: Scale<f64>,
) -> Vec<u32>
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Send + Clone + 'static,
{
    let mut flying = Vec::new();
    if !lxb.restores.flying(output, now) {
        return flying;
    }
    let space = &lxb.space;
    let Some(output_geo) = space.output_geometry(output) else {
        return flying;
    };

    for window in space.elements_for_output(output).rev() {
        let id = crate::overview::window_id(window);
        let Some((from, left)) = lxb.restores.flight(output, id, now) else {
            continue;
        };
        let Some(geometry) = space.element_geometry(window) else {
            continue;
        };
        if geometry.size.w <= 0 {
            continue;
        }
        let current = Rectangle::<f64, smithay::utils::Logical>::new(
            (geometry.loc - output_geo.loc).to_f64(),
            geometry.size.to_f64(),
        );
        let start =
            lxb_protocol::overview::fit(&from, geometry.size.w as f64, geometry.size.h as f64);

        let x = current.loc.x + (start.x - current.loc.x) * left;
        let y = current.loc.y + (start.y - current.loc.y) * left;
        let w = current.size.w + (start.w - current.size.w) * left;
        let window_scale = w / current.size.w;

        let corner = smithay::utils::Point::<f64, smithay::utils::Logical>::from((x, y));
        let anchor = corner.to_physical(scale).to_i32_round();
        let location = (corner - window.geometry().loc.to_f64())
            .to_physical(scale)
            .to_i32_round();
        elements.extend(
            window
                .render_elements::<WaylandSurfaceRenderElement<R>>(renderer, location, scale, 1.0)
                .into_iter()
                .map(|element| {
                    LxbRenderElement::OverviewCard(RescaleRenderElement::from_element(
                        element,
                        anchor,
                        window_scale,
                    ))
                }),
        );
        flying.push(id);
    }
    flying
}

/// Draw the overview's windows, each `progress` of the way between its real
/// geometry and its aspect-fitted card.
///
/// The card slots come from the layout shared with the shell, so the frames
/// and titles the shell paints on its overlay land exactly on the windows
/// drawn here.
fn push_overview_windows<R>(
    elements: &mut Vec<LxbRenderElement<R>>,
    renderer: &mut R,
    lxb: &Lxb,
    output: &Output,
    progress: f64,
    scale: Scale<f64>,
) where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Send + Clone + 'static,
{
    let space = &lxb.space;
    let Some(output_geo) = space.output_geometry(output) else {
        return;
    };

    let windows = overview_windows(lxb, output);
    // One extra card: the trailing start-screen card the shell appends. It
    // occupies a slot (so counting and scrolling agree with the shell) but
    // no window is drawn there — the shell paints it.
    let count = windows.len() + 1;
    let selected = lxb.overview.selection(output);
    let slots = lxb_protocol::overview::card_slots(
        output_geo.size.w as f64,
        output_geo.size.h as f64,
        count,
        selected,
    );

    let dt = lxb.overview.tick(output, std::time::Instant::now());

    for (window, slot) in windows.iter().zip(&slots) {
        // Cards hanging past the screen edges are drawn too — clipped by the
        // output, they are the peek that says the column scrolls.
        let Some(geometry) = space.element_geometry(window) else {
            continue;
        };
        let current = Rectangle::<f64, smithay::utils::Logical>::new(
            (geometry.loc - output_geo.loc).to_f64(),
            geometry.size.to_f64(),
        );
        let target =
            lxb_protocol::overview::fit(slot, geometry.size.w as f64, geometry.size.h as f64);
        // The slot end of the flight eases towards its place, which is what
        // turns a scroll of the row into a glide instead of a teleport.
        let seat = lxb
            .overview
            .glide(output, crate::overview::window_id(window), target, dt);

        // Both endpoints share the window's aspect ratio, so interpolating
        // the corners keeps it too and the scale below is uniform.
        let x = current.loc.x + (seat.x - current.loc.x) * progress;
        let y = current.loc.y + (seat.y - current.loc.y) * progress;
        let w = current.size.w + (seat.w - current.size.w) * progress;
        if current.size.w <= 0.0 {
            continue;
        }
        let window_scale = w / current.size.w;

        let corner = smithay::utils::Point::<f64, smithay::utils::Logical>::from((x, y));
        // The corner of the card is the corner of the window's geometry, so it
        // is what the shrink is anchored to; the surface itself starts a
        // shadow's width before it, exactly as on the desktop above.
        let anchor = corner.to_physical(scale).to_i32_round();
        let location = (corner - window.geometry().loc.to_f64())
            .to_physical(scale)
            .to_i32_round();
        elements.extend(
            window
                .render_elements::<WaylandSurfaceRenderElement<R>>(renderer, location, scale, 1.0)
                .into_iter()
                .map(|element| {
                    LxbRenderElement::OverviewCard(RescaleRenderElement::from_element(
                        element,
                        anchor,
                        window_scale,
                    ))
                }),
        );
    }
}

/// Release frame callbacks for everything actually on screen on `output`.
///
/// Clients block on these before drawing their next frame, so a backend that
/// forgets to call this will appear to hang every client on that output. That
/// is also what makes this the place to decide who *should* be drawing.
/// LineXinBar tiles each application across its whole display, so a window with
/// another one in front of it is not partly visible — it is not on screen at
/// all, and a client still being asked for frames there goes on decoding video
/// nobody can watch. Withholding the callback takes it to no frames at all
/// until it comes back to the front, which is the whole saving: the front
/// application being translucent changes what the user can see through it, not
/// whether the window behind is worth drawing.
///
/// Two things are exempt. The home menu, because its overview shows every
/// window on the display at once and those cards are the live windows rather
/// than screenshots, so while it is up they all draw again. And every layer
/// surface, for the reason given where they are sent below.
pub fn post_repaint(
    lxb: &Lxb,
    output: &Output,
    time: std::time::Duration,
    throttle: Option<std::time::Duration>,
) {
    let space = &lxb.space;
    // Read off the flight rather than a flag, so windows are already running
    // by the time they arrive in their cards and keep running until the last
    // one has flown home.
    let overview_up = lxb.overview.progress(output, std::time::Instant::now()) > 0.0;
    let front = (!overview_up)
        .then(|| front_application(lxb, output))
        .flatten();
    let cover = front
        .as_ref()
        .and_then(|window| space.element_geometry(window));

    // Whatever is answered here was never put on the screen, and its client has
    // to be told so rather than left waiting: see [`answer_unshown`].
    let mut unshown = OutputPresentationFeedback::new(output);

    let mut behind_front = false;
    for window in space.elements_for_output(output).rev() {
        // An application the shell is driving rather than showing goes on
        // drawing at its own pace, and is neither in front of anything nor
        // covering it. Both halves matter. It is not on screen, but it is the
        // thing the shell is waiting on — a client held in its own present
        // never answers, and Valve's is asked to start the game from the same
        // thread it draws on. And because it is never the front application,
        // the game *behind* it goes on being sent frames rather than being
        // treated as covered by a window nobody can see.
        if lxb.out_of_sight(window) {
            window.send_frame(output, time, throttle, |_, _| Some(output.clone()));
            answer_unshown(window, &mut unshown, output);
            continue;
        }
        // Topmost first. Everything down to the application in front is on
        // screen, chrome stacked above it included; below it, only what that
        // application leaves uncovered.
        if behind_front && covered(cover, space.element_geometry(window)) {
            continue;
        }
        behind_front |= Some(window) == front.as_ref();
        window.send_frame(output, time, throttle, |_, _| Some(output.clone()));
    }

    // And every window that is on no display at all.
    //
    // The loop above asks the space which of its windows belong to this output,
    // and a window lying outside every one of them belongs to none — so without
    // this it is never sent a frame by anybody, whichever display draws. That
    // is not a corner case: Valve's client creates its notification toasts and
    // its popups far off the screen (805240832, 805240832) and only moves them
    // on when it wants them seen, which is a thing X11 clients do all the time.
    //
    // A client that is never told its frame went out stops drawing, and one
    // whose frames go through Xwayland stops *presenting*, which blocks the
    // thread that asked. Steam composites every one of its windows on a single
    // thread, so one toast nobody can see is enough to stop the whole client —
    // including the part of it that starts games, which is how this was found:
    // a press handed to the client, a "Launching" window that never painted,
    // and a game that never started.
    //
    // Done from every output's pass rather than one chosen one. A frame
    // callback can only be answered once, so the second pass costs nothing,
    // and choosing a display would mean choosing one that may be asleep.
    for window in space.elements() {
        if !space.outputs_for_element(window).is_empty() {
            continue;
        }
        window.send_frame(output, time, throttle, |_, _| Some(output.clone()));
        answer_unshown(window, &mut unshown, output);
    }

    // Nothing collected above was drawn, by definition. Saying so is what frees
    // a client that is waiting to hear what happened to the frame it gave us.
    unshown.discarded();

    // Layer surfaces always draw, covered or not, and the background ones
    // behind a fullscreen application are exactly the tempting case to get
    // this wrong on. The session shell is a layer-shell client; it presents
    // FIFO, which paces on these callbacks, and it runs one thread. Withhold
    // them and it blocks inside its own present rather than idling — so it
    // never hears the guide button, and a session whose shell cannot be
    // summoned back over the application in front of it is over. It already
    // stops drawing by itself when something covers it, which is the saving
    // this would have been for.
    let map = layer_map_for_output(output);
    for layer in map.layers() {
        layer.send_frame(output, time, throttle, |_, _| Some(output.clone()));
    }
}

/// Collect what one never-drawn window asked to be told about its frame.
///
/// A client may ask, through `wp_presentation`, when the frame it just gave the
/// compositor reached the screen. The answer for these windows is that it never
/// did and never will, and the caller says so by discarding what this collects.
/// Left uncollected the request simply stays queued for ever — and a client that
/// paces itself on the answer, as an accelerated Xwayland client does, waits
/// with it.
///
/// The output is forced rather than read off the surface for the reason the
/// frame callback beside it is: what smithay records there is where the surface
/// was last *scanned out*, and a window that is never drawn has no such place.
fn answer_unshown(window: &Window, unshown: &mut OutputPresentationFeedback, output: &Output) {
    window.take_presentation_feedback(
        unshown,
        |_, _| Some(output.clone()),
        |surface, _| {
            smithay::desktop::utils::surface_presentation_feedback_flags_from_states(
                surface,
                &Default::default(),
            )
        },
    );
}

/// The application in front on `output`: the topmost window that takes
/// keyboard focus, which under LineXinBar's one-application-per-display layout
/// is the one filling it.
///
/// The topmost one rather than the *focused* one, for the reason
/// [`crate::shell_control`] names the foreground the same way: while the
/// shell's overlay is up it holds the keyboard itself, and the application it
/// is drawn over has not gone anywhere.
///
/// Whatever the shell is keeping out of sight is not in front of anything: it
/// is not on the screen to be in front *of*, and calling it the front
/// application would take the frames away from the one that is.
fn front_application(lxb: &Lxb, output: &Output) -> Option<Window> {
    lxb.space
        .elements_for_output(output)
        .rev()
        .find(|window| window_accepts_keyboard_focus(window) && !lxb.out_of_sight(window))
        .cloned()
}

/// Whether something at `geometry` is completely hidden by the application in
/// front of it.
///
/// `cover` is that application's geometry, and is `None` when there is nothing
/// in front — an empty display, or one showing the overview, where everything
/// draws. Anything the application only partly overlaps keeps drawing too. A
/// bar reserving space at the edge of the display tiles the application into
/// what is left, and a window standing out past that is a window with pixels
/// of its own on screen.
fn covered(
    cover: Option<Rectangle<i32, Logical>>,
    geometry: Option<Rectangle<i32, Logical>>,
) -> bool {
    match (cover, geometry) {
        (Some(cover), Some(geometry)) => cover.contains_rect(geometry),
        // Nothing in front, or nothing placed to hide: left drawing rather
        // than starved on a guess.
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::covered;
    use smithay::utils::{Logical, Rectangle};

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Logical> {
        Rectangle::new((x, y).into(), (w, h).into())
    }

    #[test]
    fn a_tiled_application_hides_what_is_behind_it() {
        // Both windows tiled across the same display, which is the ordinary
        // case: one game, one terminal opened over it.
        let display = rect(0, 0, 1920, 1080);
        assert!(covered(Some(display), Some(display)));
    }

    #[test]
    fn nothing_in_front_hides_nothing() {
        assert!(!covered(None, Some(rect(0, 0, 1920, 1080))));
    }

    #[test]
    fn a_partly_overlapped_window_keeps_drawing() {
        // A bar reserving the top of the display tiles the application below
        // it, so a window standing up into that strip is still on screen.
        let application = rect(0, 40, 1920, 1040);
        let taller = rect(0, 0, 1920, 1080);
        assert!(!covered(Some(application), Some(taller)));
    }

    #[test]
    fn a_window_on_another_display_is_not_hidden() {
        let application = rect(0, 0, 1920, 1080);
        let elsewhere = rect(1920, 0, 1920, 1080);
        assert!(!covered(Some(application), Some(elsewhere)));
    }
}
