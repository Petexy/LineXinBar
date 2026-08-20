//! Render element assembly, shared by every backend.
//!
//! Both the nested and the DRM backend need the exact same element list for a
//! given output, so it is built once here and is generic over the renderer.

use smithay::backend::renderer::element::memory::MemoryRenderBufferRenderElement;
use smithay::backend::renderer::element::solid::SolidColorRenderElement;
use smithay::backend::renderer::element::surface::WaylandSurfaceRenderElement;
use smithay::backend::renderer::element::utils::RescaleRenderElement;
use smithay::backend::renderer::element::{
    default_primary_scanout_output_compare, AsRenderElements, Kind, RenderElementStates,
};
use smithay::backend::renderer::utils::CommitCounter;
use smithay::backend::renderer::{ImportAll, ImportMem, Renderer};
use smithay::desktop::utils::{update_surface_primary_scanout_output, OutputPresentationFeedback};
use smithay::desktop::{layer_map_for_output, Window};
use smithay::output::Output;
use smithay::reexports::wayland_server::Resource;
use smithay::utils::{Logical, Rectangle, Scale};
use smithay::wayland::seat::WaylandFocus;
use smithay::wayland::shell::wlr_layer::Layer;

use crate::input::window_accepts_keyboard_focus;
use crate::state::Lxb;

pub use crate::cursor::CursorState;

smithay::render_elements! {
    /// Everything LineXinBar can put on screen.
    pub LxbRenderElement<R> where R: ImportAll + ImportMem;
    /// A client surface (window, layer surface, popup or cursor surface).
    Surface = WaylandSurfaceRenderElement<R>,
    /// A window drawn at some size other than the one it was configured at:
    /// mid-flight in the overview, on its way into or out of a card, or an
    /// application drawing larger than life for [`crate::scale`].
    Scaled = RescaleRenderElement<WaylandSurfaceRenderElement<R>>,
    /// A CPU-side image: the themed cursor.
    Memory = MemoryRenderBufferRenderElement<R>,
    /// One flat colour over the whole display: the flash a screenshot answers
    /// with, and the black a session goes out behind.
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

    // The curtain over everything, the cursor and the flash included: a session
    // on its way out takes the whole screen with it, and a pointer left drawn
    // over the black would be the last thing anybody saw of it. Nothing else
    // in this list is above it — see [`crate::curtain`].
    if let Some((id, black, commit)) = lxb.curtain.black(now) {
        elements.push(LxbRenderElement::Solid(SolidColorRenderElement::new(
            id,
            Rectangle::from_size(output_geo.size.to_physical_precise_round(scale)),
            commit,
            // Premultiplied, as below: black at this alpha is zero in the
            // colour channels and the alpha itself in the fourth.
            [0.0, 0.0, 0.0, black],
            Kind::Unspecified,
        )));
    }

    // Then the black one display rests behind while a game is played on
    // another — under the curtain, because the session leaving is over every
    // screen and outranks one screen sleeping, and over everything else on
    // this one for the reason the curtain is over everything: a cursor left
    // lit on a resting OLED panel is the brightest thing on it. See
    // [`crate::blackout`].
    if let Some((id, black, commit)) = lxb.blackouts.black(output, now) {
        elements.push(LxbRenderElement::Solid(SolidColorRenderElement::new(
            id,
            Rectangle::from_size(output_geo.size.to_physical_precise_round(scale)),
            commit,
            // Premultiplied, as the curtain above is.
            [0.0, 0.0, 0.0, black],
            Kind::Unspecified,
        )));
    }

    // Then the flash, cursor included: a camera's answer is over the whole
    // screen or it is not an answer. It is never in the photograph — the
    // picture is read back before the flash is started — and it is only ever
    // here for the few frames after one was actually written.
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
            let surfaces = window
                .render_elements::<WaylandSurfaceRenderElement<R>>(renderer, relative, scale, 1.0);
            // The third part of drawing an application larger than life: it was
            // configured at a fraction of the display and told to fill that
            // fraction with the display's own pixels, so what comes back has to
            // be laid over the whole of it again. Anchored at the window's own
            // corner, which is where the display's usable area begins — the
            // shadow spilling before it grows with the window, as it does when
            // the same window shrinks into an overview card below.
            //
            // A client that honoured the scale is then drawn pixel for pixel:
            // its buffer already has as many pixels as the rectangle this puts
            // it in. One that did not is enlarged, softly, which is the same
            // answer it gets from every compositor.
            let factor = crate::scale::window_scale(lxb.outputs.app_scale(), window);
            if factor == 1.0 {
                elements.extend(surfaces.into_iter().map(LxbRenderElement::Surface));
                continue;
            }
            let anchor = (location - output_geo.loc).to_physical_precise_round(scale);
            elements.extend(surfaces.into_iter().map(|element| {
                LxbRenderElement::Scaled(RescaleRenderElement::from_element(
                    element, anchor, factor,
                ))
            }));
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
        // Where the window is on screen, which for an application drawing
        // larger than life is not the rectangle it was configured at: the
        // flight has to start from what the user can see, or a window scaled
        // to 150% would jump to two thirds of its size before setting off.
        let factor = crate::scale::window_scale(lxb.outputs.app_scale(), window);
        let current = Rectangle::<f64, smithay::utils::Logical>::new(
            (geometry.loc - output_geo.loc).to_f64(),
            crate::scale::visual_geometry(geometry, factor)
                .size
                .to_f64(),
        );
        let start =
            lxb_protocol::overview::fit(&from, geometry.size.w as f64, geometry.size.h as f64);

        let x = current.loc.x + (start.x - current.loc.x) * left;
        let y = current.loc.y + (start.y - current.loc.y) * left;
        let w = current.size.w + (start.w - current.size.w) * left;
        // Against the size the client actually drew, not against where the
        // flight began: the element's own pixels are the window's configured
        // size, and at rest this comes back to the plain application scale.
        let window_scale = w / geometry.size.w as f64;

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
                    LxbRenderElement::Scaled(RescaleRenderElement::from_element(
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
        // Where it is on screen, which is its configured rectangle grown by
        // however much larger than life its application is drawing — see the
        // flight home in [`push_restoring_windows`], which starts from the same
        // rectangle this one ends at.
        let factor = crate::scale::window_scale(lxb.outputs.app_scale(), window);
        let current = Rectangle::<f64, smithay::utils::Logical>::new(
            (geometry.loc - output_geo.loc).to_f64(),
            crate::scale::visual_geometry(geometry, factor)
                .size
                .to_f64(),
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
        if current.size.w <= 0.0 || geometry.size.w <= 0 {
            continue;
        }
        // Against what the client drew rather than against where the card
        // started, for the reason given on the flight home.
        let window_scale = w / geometry.size.w as f64;

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
                    LxbRenderElement::Scaled(RescaleRenderElement::from_element(
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
/// Three things are exempt. The home menu, because its overview shows every
/// window on the display at once and those cards are the live windows rather
/// than screenshots, so while it is up they all draw again. Every layer
/// surface, for the reason given where they are sent below. And every other
/// window of the application in front, for the reason given at [`covered`]'s
/// use below — the saving is only ever worth having between applications.
pub fn post_repaint(
    lxb: &Lxb,
    output: &Output,
    time: std::time::Duration,
    throttle: Option<std::time::Duration>,
    drawn: &RenderElementStates,
) {
    // First, and before anything reads it back: record which display each
    // surface was just drawn on. See [`record_where_each_surface_was_drawn`] —
    // a frame nobody records the display of is a frame the compositor cannot
    // afterwards tell a client it showed.
    record_where_each_surface_was_drawn(lxb, output, drawn);

    let space = &lxb.space;
    // Who has something on this display, worked out once — see
    // [`windows_on_screen`], which is this decision and is asked the same
    // question by [`crate::sleep`].
    let shown = windows_on_screen(lxb, output);
    // And which of them is the one in front, for the watch below. Not while
    // the overview is up: every window is drawing there and none of them is
    // the display.
    let front = (lxb.overview.progress(output, std::time::Instant::now()) == 0.0)
        .then(|| front_application_on_screen(lxb, output))
        .flatten();
    if let Some(front) = &front {
        watch_for_a_quiet_application(lxb, front);
    }

    // Whatever is answered here was never put on the screen, and its client has
    // to be told so rather than left waiting: see [`answer_unshown`].
    let mut unshown = OutputPresentationFeedback::new(output);

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
        if !shown.contains(window) {
            // Not on screen, and its client has to hear that rather than wait
            // for a frame that was never shown: see [`answer_unshown`].
            answer_unshown(window, &mut unshown, output);
            continue;
        }
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

/// Every window with something of itself on `output` at this moment.
///
/// The one place the question is decided. [`post_repaint`] sends a frame to
/// exactly this list and tells everything else on the display that its frame
/// was never shown; [`crate::sleep`] stops the applications that are on none of
/// these lists at all. Two answers to *is this on screen* is the way a game
/// ends up stopped in front of somebody, so there is one.
///
/// Topmost first. Everything down to the application in front is on screen,
/// chrome stacked above it included; below it, only what that application
/// leaves uncovered.
///
/// Except the front application's own other windows, which count as on screen
/// however thoroughly they are hidden. An application is not a window: it is
/// one client, drawing every window it has from threads it shares between
/// them, so a frame withheld from the one nobody can see is withheld from the
/// thread that draws the one they can. A game under Proton is the case that
/// proves it — `winewayland.drv` gives every window the game opens a toplevel
/// of its own, this shell tiles each of them over the whole display, and the
/// newest one is in front. Starve what it covers and the game's render thread
/// stops in its own present, still holding the frame the screen is waiting for,
/// until the engine gives up on it and takes the game down with it. The saving
/// this is for is a whole application nobody is looking at; there is none to be
/// had inside the one they are.
///
/// Two answers are the whole display at once. While the shell is painting over
/// it nothing of anybody's is on screen — its own windows included, since there
/// is no application in front for them to belong to. While the overview is up
/// everything is, because those cards are the live windows themselves.
///
/// A window the shell is driving out of sight is on no list. It is not on the
/// screen, which is the question this answers; that it must still be sent
/// frames is [`post_repaint`]'s own business, and that it must never be put to
/// sleep is [`crate::sleep`]'s.
pub fn windows_on_screen(lxb: &Lxb, output: &Output) -> Vec<Window> {
    // Read off the flight rather than a flag, so windows are already running
    // by the time they arrive in their cards and keep running until the last
    // one has flown home.
    let overview_up = lxb.overview.progress(output, std::time::Instant::now()) > 0.0;
    if !overview_up && shell_hides_the_display(lxb, output) {
        return Vec::new();
    }
    let front = (!overview_up)
        .then(|| front_application(lxb, output))
        .flatten();
    let cover = front.as_ref().and_then(|window| on_screen(lxb, window));

    let mut shown = Vec::new();
    let mut behind_front = false;
    for window in lxb.space.elements_for_output(output).rev() {
        if lxb.out_of_sight(window) {
            continue;
        }
        if behind_front
            && covered(cover, on_screen(lxb, window))
            && !front
                .as_ref()
                .is_some_and(|front| same_application(window, front))
        {
            continue;
        }
        behind_front |= Some(window) == front.as_ref();
        shown.push(window.clone());
    }
    shown
}

/// Record, for every surface, which display it was drawn on this frame.
///
/// Nothing in the picture depends on this, and everything a client is told
/// about its own frames does. `wp_presentation` — the protocol behind
/// `VK_KHR_present_wait`, which is how a Vulkan client asks to be woken when
/// its frame actually reaches the screen — is answered by collecting each
/// surface's request at the display it was scanned out on, and the *only*
/// record of which display that was is the one written here. Without it every
/// surface answers "nowhere", nothing is collected, and the page flip that
/// carries a frame to the screen tells nobody it did.
///
/// What the client sees then is worse than silence. Its request is not lost, it
/// is superseded by the next frame it commits, and the answer it eventually
/// gets is `discarded` — this frame was never shown — for a frame that was on
/// the screen for a whole refresh. A client that only ever draws forward is
/// merely lied to about its own timing. One that waits for the answer before
/// drawing the next frame is deadlocked: the answer to frame N arrives only
/// with the commit of frame N+1, which is the commit it is waiting to make.
/// That is a game's render thread parked in `vkQueuePresentKHR` for good, with
/// no fault, no error and nothing in this log — and an engine that gives its
/// render thread two minutes before it takes the process down.
///
/// Every window, not only the ones on this display: a surface may be drawn on
/// several, and Smithay keeps the record per display and picks between them.
fn record_where_each_surface_was_drawn(lxb: &Lxb, output: &Output, drawn: &RenderElementStates) {
    for window in lxb.space.elements() {
        window.with_surfaces(|surface, states| {
            update_surface_primary_scanout_output(
                surface,
                output,
                states,
                drawn,
                default_primary_scanout_output_compare,
            );
        });
    }
    for layer in layer_map_for_output(output).layers() {
        layer.with_surfaces(|surface, states| {
            update_surface_primary_scanout_output(
                surface,
                output,
                states,
                drawn,
                default_primary_scanout_output_compare,
            );
        });
    }
}

/// Collect what everything drawn on `output` asked to be told about its frame.
///
/// Each surface is taken at the display it was scanned out on, which
/// [`post_repaint`] must have recorded for this frame first. `drawn` also
/// decides the flags the answer carries: whether the client's own buffer
/// reached the screen untouched, and whether the timestamp came from hardware.
pub fn collect_presentation_feedback(
    lxb: &Lxb,
    output: &Output,
    drawn: &RenderElementStates,
) -> OutputPresentationFeedback {
    let mut feedback = OutputPresentationFeedback::new(output);
    for window in lxb.space.elements_for_output(output) {
        window.take_presentation_feedback(
            &mut feedback,
            smithay::desktop::utils::surface_primary_scanout_output,
            |surface, _| {
                smithay::desktop::utils::surface_presentation_feedback_flags_from_states(
                    surface, drawn,
                )
            },
        );
    }
    for layer in layer_map_for_output(output).layers() {
        layer.take_presentation_feedback(
            &mut feedback,
            smithay::desktop::utils::surface_primary_scanout_output,
            |surface, _| {
                smithay::desktop::utils::surface_presentation_feedback_flags_from_states(
                    surface, drawn,
                )
            },
        );
    }
    feedback
}

/// Answer that feedback straight away, for a backend that has no vertical blank
/// of its own to answer it at.
///
/// The nested backends draw into a window belonging to somebody else's
/// compositor, so the moment their frame reaches a screen is not something they
/// are told. What they do know is that the frame has been handed over, and that
/// is what is said here — without the vsync and hardware-clock flags a real
/// page flip earns, because neither is true of a frame given to another
/// compositor.
///
/// Saying nothing is not the alternative it looks like. A client that waits for
/// this answer — anything using `VK_KHR_present_wait`, which is most Vulkan
/// games — stops dead without it, and a shell developed against a nested
/// session would meet that the moment it was run for real.
pub fn answer_presentation_now(
    lxb: &Lxb,
    output: &Output,
    drawn: &RenderElementStates,
    time: std::time::Duration,
) {
    use smithay::reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback;
    let mut feedback = collect_presentation_feedback(lxb, output, drawn);
    let refresh = output
        .current_mode()
        .map(|mode| {
            smithay::wayland::presentation::Refresh::fixed(std::time::Duration::from_secs_f64(
                1000.0 / mode.refresh as f64,
            ))
        })
        .unwrap_or(smithay::wayland::presentation::Refresh::Unknown);
    feedback.presented::<_, smithay::utils::Monotonic>(
        time,
        refresh,
        0,
        wp_presentation_feedback::Kind::empty(),
    );
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

/// Whether this display's next page flip may be immediate rather than waiting
/// for the vertical retrace.
///
/// Tearing belongs to the scanout rather than to a window: there is one flip
/// for the display, and it either waits or it does not. So the question is not
/// "does some surface want to tear" but "is the whole picture one surface that
/// does" — anything drawn over the application would be torn along with it, and
/// the shell's own chrome is not something a game's client may choose to tear.
///
/// Which makes every one of these a reason to wait:
///
/// * The overview being up, or anything else the shell has drawn over the
///   application: the guide, the volume bar, a notification. All of them arrive
///   as layer surfaces above the window, and none belongs to the client that
///   asked.
/// * The application not covering the display, so the wallpaper or another
///   window is on screen beside it.
/// * The front application not having asked, which is nearly every one of them.
///
/// See [`crate::tearing`] for what the client asked and how.
pub fn output_may_tear(lxb: &Lxb, output: &Output) -> bool {
    if lxb.overview.progress(output, std::time::Instant::now()) > 0.0 {
        return false;
    }
    // Anything the shell has put on screen above the application. The
    // background layers are underneath it and do not count: a fullscreen
    // application covers them, and they are not drawn.
    let map = layer_map_for_output(output);
    let chrome_above = map
        .layers_on(Layer::Overlay)
        .chain(map.layers_on(Layer::Top))
        .any(|layer| {
            map.layer_geometry(layer)
                .is_some_and(|geometry| !geometry.size.is_empty())
        });
    if chrome_above {
        return false;
    }
    drop(map);

    let Some(front) = front_application(lxb, output) else {
        return false;
    };
    // The application has to *be* the picture. `covered` asks this question the
    // other way round everywhere else in this file; here the display's own area
    // is what has to be covered.
    let Some(area) = lxb.space.output_geometry(output) else {
        return false;
    };
    if on_screen(lxb, &front).is_none_or(|geometry| !geometry.contains_rect(area)) {
        return false;
    }
    front
        .wl_surface()
        .is_some_and(|surface| crate::tearing::surface_wants_tearing(&surface))
}

/// Whether this display is showing content that is already encoded the way the
/// cable is, so [`crate::hdr`]'s colour pipeline must leave it alone.
///
/// True only for a frame that is one client's buffer and nothing else: a
/// colour-managed surface saying its pixels are ST 2084, filling the display,
/// with nothing drawn over it. Every part of that matters. The pipeline is the
/// *display's* — it reaches everything scanned out — so the moment the shell
/// puts a single pixel on top, that pixel is sRGB and needs the encode the
/// pipeline normally does. There is no middle setting: either the frame is the
/// client's own encoding or it is this compositor's.
///
/// Which is the same shape as [`output_may_tear`], and for the same underlying
/// reason: both are properties of the scanout rather than of a window, so both
/// ask whether one surface *is* the picture rather than whether it is in it.
pub fn output_shows_encoded_content(lxb: &Lxb, output: &Output) -> bool {
    if lxb.overview.progress(output, std::time::Instant::now()) > 0.0 {
        return false;
    }
    let map = layer_map_for_output(output);
    let chrome_above = map
        .layers_on(Layer::Overlay)
        .chain(map.layers_on(Layer::Top))
        .any(|layer| {
            map.layer_geometry(layer)
                .is_some_and(|geometry| !geometry.size.is_empty())
        });
    if chrome_above {
        return false;
    }
    drop(map);

    let Some(front) = front_application(lxb, output) else {
        return false;
    };
    let Some(area) = lxb.space.output_geometry(output) else {
        return false;
    };
    if on_screen(lxb, &front).is_none_or(|geometry| !geometry.contains_rect(area)) {
        return false;
    }
    front
        .wl_surface()
        .is_some_and(|surface| crate::colour::surface_colour(&surface).is_hdr())
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
/// The application on `output` that the user can actually see, which is the
/// front one unless the shell is painting over the display — see
/// [`shell_hides_the_display`]. `None` for a display with nothing on it, and
/// for one the shell has covered.
///
/// The same question the frame throttle asks, answered in the same place, so
/// that what an application is *told* about being on screen and what it is
/// *given* to draw with cannot come apart. They did: a window can be told it is
/// deactivated and go on being handed frames, which is a game that has paused
/// itself in front of a user who is looking straight at it.
pub fn front_application_on_screen(lxb: &Lxb, output: &Output) -> Option<Window> {
    if shell_hides_the_display(lxb, output) {
        return None;
    }
    front_application(lxb, output)
}

fn front_application(lxb: &Lxb, output: &Output) -> Option<Window> {
    lxb.space
        .elements_for_output(output)
        .rev()
        .find(|window| window_accepts_keyboard_focus(window) && !lxb.out_of_sight(window))
        .cloned()
}

/// The rectangle a window actually occupies on screen.
///
/// Its mapped geometry, grown by however much larger than life its application
/// is drawing — see [`crate::scale`]. Everything that asks whether a window
/// covers something has to ask it of this rather than of the geometry the client
/// was configured at: those two are the same rectangle on an ordinary session
/// and differ by the whole of this setting on a scaled one, and a frame throttle
/// reading the wrong one would decide that no application ever fills a screen
/// the moment somebody asked for larger windows.
fn on_screen(lxb: &Lxb, window: &Window) -> Option<Rectangle<i32, Logical>> {
    let geometry = lxb.space.element_geometry(window)?;
    let factor = crate::scale::window_scale(lxb.outputs.app_scale(), window);
    Some(crate::scale::visual_geometry(geometry, factor))
}

/// Whether the session shell is painting over the whole of `output`, hiding
/// every application on it.
///
/// The other way an application stops being on screen, and until this the only
/// one the compositor knew about was another application. A game with the start
/// screen over it is exactly as invisible as a game with a browser over it, and
/// it went on drawing at whatever rate it liked and playing its music into a
/// room where nobody could see it — which is the complaint this is here for.
///
/// Asked of the shell's own declaration rather than guessed at. A surface says
/// what is behind it can be thrown away by setting an opaque region, and the
/// shell sets one exactly when its bar is standing over an application and
/// carrying the whole picture itself. It is the *only* thing that can answer
/// this: the guide's glass and the loading screen mid-grow are the same surface
/// at the same size, and both are deliberately see-through — the guide because
/// its cards are the live windows themselves, the splash because the window it
/// is waiting for maps underneath it and is revealed rather than switched to.
///
/// The declaration is about the surface and not about the display, which is not
/// a nicety: a region is a rendering instruction as much as it is an answer to
/// this, and everything behind one is discarded. The shell's start screen paints
/// its display across *two* surfaces — a transparent bar over an always-
/// background wallpaper, so that the overview's live windows can be drawn
/// between them — and a bar that claimed the display would be a bar that threw
/// away its own waves. See `paints_over_everything` in the shell.
///
/// Only the two layers above the windows count. The bar spends most of its life
/// on the background layer *behind* the application, and a cover found there
/// would be one nobody can see.
fn shell_hides_the_display(lxb: &Lxb, output: &Output) -> bool {
    let Some(geometry) = lxb.space.output_geometry(output) else {
        return false;
    };
    let map = layer_map_for_output(output);
    let covered = map
        .layers()
        .filter(|layer| matches!(layer.layer(), Layer::Top | Layer::Overlay))
        .any(|layer| {
            let Some(at) = map.layer_geometry(layer) else {
                return false;
            };
            opaque_region(layer.wl_surface())
                .is_some_and(|region| region_hides(&region, at.loc, geometry))
        });
    covered
}

/// Whether an opaque region standing at `at` covers the whole of `geometry`.
///
/// One rectangle of it has to, on its own. Two that only cover the display
/// between them are a surface drawn in halves, and a compositor that added them
/// up would be guessing about the seam; nothing the shell declares is ever
/// shaped like that, and the cost of being wrong here is an application put to
/// sleep in front of somebody.
fn region_hides(
    region: &[Rectangle<i32, Logical>],
    at: smithay::utils::Point<i32, Logical>,
    geometry: Rectangle<i32, Logical>,
) -> bool {
    region.iter().any(|rect| {
        // In the display's coordinates: a region is the client's own, and its
        // surface stands where the layer map put it.
        let mut rect = *rect;
        rect.loc += at;
        rect.contains_rect(geometry)
    })
}

/// The rectangles a surface has declared it draws opaquely, if any.
///
/// Only the added ones. A region is built by adding and subtracting, and a
/// subtraction can only ever make the answer smaller — so ignoring them would
/// let a hole in the middle of a surface be read as solid. Any subtraction at
/// all therefore gives up rather than approximating.
fn opaque_region(
    surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
) -> Option<Vec<Rectangle<i32, Logical>>> {
    use smithay::wayland::compositor::{with_states, RectangleKind, SurfaceAttributes};
    with_states(surface, |states| {
        let mut attributes = states.cached_state.get::<SurfaceAttributes>();
        let region = attributes.current().opaque_region.clone()?;
        region
            .rects
            .iter()
            .map(|(kind, rect)| match kind {
                RectangleKind::Add => Some(*rect),
                RectangleKind::Subtract => None,
            })
            .collect()
    })
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

/// How long the application in front may go without drawing a frame before the
/// log says so.
///
/// Long enough that a stall this short is something the user would not notice,
/// and short enough to land in the log well before an engine's own watchdog
/// fires: Unreal gives its render thread two minutes, so a line here comes
/// nearly two minutes before the game dies, with the compositor's side of the
/// story attached to it.
const QUIET: std::time::Duration = std::time::Duration::from_secs(5);

/// Whether `window` has put a frame up within `recently`.
///
/// The same reading [`watch_for_a_quiet_application`] takes, asked for a
/// different purpose: that one logs a game whose render thread has parked, and
/// this one answers whether a display has anything moving on it — which is what
/// tells a screen that can be rested from one somebody is watching. See
/// [`crate::blackout`] and `lxb_shell_v1.output_drawing`.
///
/// A window that has never committed anything has no reading at all, and is
/// counted as still: it has shown nobody anything.
pub fn painted_within(window: &Window, recently: std::time::Duration) -> bool {
    window
        .user_data()
        .get::<LastDrawn>()
        .is_some_and(|drawn| drawn.at.get().elapsed() < recently)
}

/// When a window last had a frame of its own to show, and whether its silence
/// has already been reported.
///
/// Kept on the window rather than in a table beside it so that it goes when the
/// window does.
#[derive(Debug)]
pub struct LastDrawn {
    at: std::cell::Cell<std::time::Instant>,
    reported: std::cell::Cell<bool>,
}

impl Default for LastDrawn {
    fn default() -> Self {
        Self {
            at: std::cell::Cell::new(std::time::Instant::now()),
            reported: std::cell::Cell::new(false),
        }
    }
}

/// Note that `window` has just committed something to show.
pub fn drew(window: &Window) {
    window.user_data().insert_if_missing(LastDrawn::default);
    let Some(drawn) = window.user_data().get::<LastDrawn>() else {
        return;
    };
    let since = drawn.at.replace(std::time::Instant::now());
    if drawn.reported.replace(false) {
        tracing::info!(
            app_id = crate::shell_control::window_app_id(window),
            after = ?since.elapsed(),
            "the application in front is drawing again"
        );
    }
}

/// Say so when the application in front stops drawing while this compositor is
/// still asking it to.
///
/// A client that stops presenting is normally its own business, and this says
/// nothing about one that is merely idle — an idle client has no frame to give
/// and is not waiting for anything. The case worth a line is the other one: a
/// game whose render thread has parked inside its own present call, waiting for
/// something this compositor was supposed to hand back. It looks identical from
/// here, so what is logged beside it is what tells the two apart — whether any
/// of this client's commits is being held by us waiting on its GPU work, and
/// for how long.
///
/// Once per silence. The line is the transition, and the one that follows it
/// when the client comes back says how long it lasted.
fn watch_for_a_quiet_application(lxb: &Lxb, window: &Window) {
    window.user_data().insert_if_missing(LastDrawn::default);
    let Some(drawn) = window.user_data().get::<LastDrawn>() else {
        return;
    };
    let quiet_for = drawn.at.get().elapsed();
    if quiet_for < QUIET || drawn.reported.replace(true) {
        return;
    }
    tracing::warn!(
        app_id = crate::shell_control::window_app_id(window),
        title = crate::shell_control::window_title(window),
        quiet_for = ?quiet_for,
        commits_held_for_its_gpu = lxb.blocked_commits,
        held_since = ?lxb.blocked_since.map(|since| since.elapsed()),
        "the application in front has stopped drawing, and is still being sent frames"
    );
}

/// Whether two windows are two windows of the same running application.
///
/// Asked of the process rather than of the name it goes by, because the name is
/// exactly what is missing in the case this exists for: a game opening a second
/// window mid-play sets no `app_id` on it more often than not, and two windows
/// that both answer "" are not thereby the same application.
///
/// The two kinds of window are asked separately, and neither answer transfers
/// to the other. Two Wayland windows are the same application when they are the
/// same client, which is what a client *is*: one connection, one process, one
/// set of threads. Two X11 windows are not, because every X11 window in the
/// session belongs to the one Xwayland connection — there the process behind
/// the window is what says, and Xwayland is asked for it. A window of each kind
/// is never a pair: a client that talks Wayland does not also talk X11 for a
/// second window of the same application.
pub(crate) fn same_application(window: &Window, other: &Window) -> bool {
    match (window.x11_surface(), other.x11_surface()) {
        (Some(window), Some(other)) => {
            // `pid` is the `_NET_WM_PID` the window advertises, which most
            // clients set and some do not; the fallback asks the X server whose
            // connection owns the window. Two windows that answer nothing at
            // all are left as separate applications rather than folded together.
            let pid = |surface: &smithay::xwayland::X11Surface| {
                surface.pid().or_else(|| surface.get_client_pid().ok())
            };
            match (pid(window), pid(other)) {
                (Some(window), Some(other)) => window == other,
                _ => false,
            }
        }
        (None, None) => match (window.wl_surface(), other.wl_surface()) {
            (Some(window), Some(other)) => window.id().same_client_as(&other.id()),
            _ => false,
        },
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{covered, region_hides};
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

    /// The shell's own screen hides an application exactly as another
    /// application does, and it is the only thing that can say so.
    ///
    /// The complaint this is here for: a game went on drawing and playing its
    /// music with the start screen painted over the whole display, because the
    /// only cover the compositor knew about was another window.
    #[test]
    fn the_shells_own_screen_hides_what_is_behind_it() {
        let display = rect(0, 0, 2560, 1440);
        let origin = (0, 0).into();

        // The start screen: one rectangle, the size of the display it is on.
        assert!(region_hides(&[rect(0, 0, 2560, 1440)], origin, display));
        // Larger than the display, which is what a surface hanging off an edge
        // declares, still hides all of it.
        assert!(region_hides(&[rect(0, 0, 3000, 1600)], origin, display));

        // The guide says nothing, because it is glass and the game behind it is
        // being looked at — in its overview, that window *is* one of the cards.
        assert!(!region_hides(&[], origin, display));

        // A panel that covers part of the screen is not the screen. Two of them
        // that only cover it between them are not either: nothing the shell
        // declares is shaped like that, and adding them up would be the
        // compositor guessing about the seam.
        assert!(!region_hides(&[rect(0, 0, 2560, 700)], origin, display));
        assert!(!region_hides(
            &[rect(0, 0, 2560, 720), rect(0, 720, 2560, 720)],
            origin,
            display
        ));

        // And a region is the client's own space, so where its surface stands
        // is added before the comparison: the same rectangle on the second
        // display hides the second display and not the first.
        let second = rect(2560, 0, 2560, 1440);
        assert!(region_hides(
            &[rect(0, 0, 2560, 1440)],
            (2560, 0).into(),
            second
        ));
        assert!(!region_hides(
            &[rect(0, 0, 2560, 1440)],
            (2560, 0).into(),
            display
        ));
    }
}
