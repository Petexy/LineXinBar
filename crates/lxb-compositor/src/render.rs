//! Render element assembly, shared by every backend.
//!
//! Both the nested and the DRM backend need the exact same element list for a
//! given output, so it is built once here and is generic over the renderer.

use smithay::backend::renderer::element::memory::MemoryRenderBufferRenderElement;
use smithay::backend::renderer::element::solid::SolidColorRenderElement;
use smithay::backend::renderer::element::surface::{
    render_elements_from_surface_tree, WaylandSurfaceRenderElement,
};
use smithay::backend::renderer::element::texture::TextureRenderElement;
use smithay::backend::renderer::element::utils::{
    CropRenderElement, Relocate, RelocateRenderElement, RescaleRenderElement,
};
use smithay::backend::renderer::element::{
    default_primary_scanout_output_compare, AsRenderElements, Element, Id, Kind,
    RenderElementStates,
};
use smithay::backend::renderer::utils::{CommitCounter, RendererSurfaceStateUserData};
use smithay::backend::renderer::{ImportAll, ImportMem, Renderer};
use smithay::desktop::utils::{update_surface_primary_scanout_output, OutputPresentationFeedback};
use smithay::desktop::{layer_map_for_output, Window};
use smithay::output::Output;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::Resource;
use smithay::utils::{Logical, Physical, Point, Rectangle, Scale};
use smithay::wayland::compositor::{with_surface_tree_downward, TraversalAction};
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
    /// The same, cut to a rectangle and then put wherever the window is
    /// standing on this frame: the floating picture-in-picture window, which
    /// may draw neither larger than its corner nor outside it. See
    /// [`push_floating_windows`] and [`Standing`].
    Floating = RelocateRenderElement<
        RescaleRenderElement<
            CropRenderElement<RescaleRenderElement<WaylandSurfaceRenderElement<R>>>,
        >,
    >,
    /// The last picture of a floating window whose client has gone, put back on
    /// the screen exactly where the window was so that it can be faded out of
    /// it. See [`crate::pip::LastPicture`].
    Kept = RelocateRenderElement<
        RescaleRenderElement<
            CropRenderElement<RescaleRenderElement<TextureRenderElement<R::TextureId>>>,
        >,
    >,
    /// A CPU-side image: the themed cursor.
    Memory = MemoryRenderBufferRenderElement<R>,
    /// The mat that rounds a floating window, standing exactly where the window
    /// inside it stands.
    Matte = RelocateRenderElement<RescaleRenderElement<MemoryRenderBufferRenderElement<R>>>,
    /// One flat colour over the whole display: the flash a screenshot answers
    /// with, and the black a session goes out behind.
    Solid = SolidColorRenderElement,
    /// And the mat's own colour behind a floating window, filling its opening —
    /// moved with the rest of the shape, and by the very same transform, so
    /// that nothing can open a seam between the two.
    Backing = RelocateRenderElement<RescaleRenderElement<SolidColorRenderElement>>,
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
        // And the floating window is left out too, for the opposite reason to
        // an unseen one: it is not something to come back to. It is already on
        // screen, it stays on screen while the cards fly, and a card offering
        // to switch to the video the user is watching over the top of this
        // very overview would be offering them nothing.
        .filter(|window| !lxb.floating(window))
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

    // Then the black one display rests behind while another one is being
    // used — under the curtain, because the session leaving is over every
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

    // In front of every other thing the session draws: the floating window, and
    // the mat that rounds it. Before the overlay layer, which is where the
    // shell's guide is — the one surface nothing else in this compositor is
    // allowed in front of, and the one this is deliberately in front of. A
    // video the user parked in a corner has to still be there while they open
    // the guide, or parking it there was pointless.
    //
    // Above the flight below it too, so a window growing back out of its tile
    // passes *under* the video rather than swallowing it.
    // The one thing allowed in front of a floating window: the surface the shell
    // draws its context menu on. See [`push_the_menu_over_floating_windows`].
    push_the_menu_over_floating_windows(&mut elements, renderer, lxb, output, scale);
    let floating = push_floating_windows(&mut elements, renderer, lxb, output, now, scale);

    let layer_map = layer_map_for_output(output);
    let a_menu_is_up = lxb.outputs.pip().has_a_menu();

    let push_layer = |elements: &mut Vec<LxbRenderElement<R>>, layer: Layer, renderer: &mut R| {
        for surface in layer_map.layers_on(layer).rev() {
            let Some(geometry) = layer_map.layer_geometry(surface) else {
                continue;
            };
            let location = geometry.loc.to_physical_precise_round(scale);
            // A tree with a menu in it is walked by hand, so the menu can be
            // left out of it: it has already been drawn, in front of the
            // floating windows. Every other surface on the session takes
            // smithay's own walk, which is the same walk without the question.
            if a_menu_is_up {
                elements.extend(
                    elements_of_all_but_the_menu(
                        renderer,
                        lxb,
                        surface.wl_surface(),
                        location,
                        scale,
                    )
                    .into_iter()
                    .map(LxbRenderElement::Surface),
                );
                continue;
            }
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

    push_windows(
        &mut elements,
        renderer,
        lxb,
        output,
        now,
        scale,
        &flying,
        &floating,
    );

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

/// How much larger than its opening a client may draw before it is scaled down
/// to fit rather than trimmed to it, in logical pixels.
///
/// A pixel, because a pixel is what rounding an opening to whole ones costs and
/// the crop takes it back for nothing. Anything more is a client drawing
/// something other than what it was asked to draw, and cropping *that* would
/// show the top left corner of a window and call it a video.
const OVERDRAWS_BY: f64 = 1.0;

/// Draw the floating window — a browser's picture-in-picture — in its corner,
/// with the mat that rounds it, and return which windows were drawn so no later
/// pass draws them again.
///
/// The mat first, because elements are drawn front to back and the mat is over
/// the window's own edges: that is what rounds the corners. See [`crate::pip`],
/// where the whole of that argument lives.
///
/// The rectangle comes from the layout rather than being worked out again here
/// — [`crate::pip::Floating::frame`] — so the mat is painted round exactly the
/// opening the client was configured into. Two halves computing the same shape
/// from the same inputs is a way of saying they can disagree, and the shape now
/// depends on a conversation with the client that only one of them is having.
///
/// **Everything is clipped to that opening, and nothing may draw larger than
/// it.** Both are about a client that is not drawing what it was asked to draw,
/// and each is a different way of not drawing it:
///
/// - A window with its own decorations puts its geometry *inside* a larger
///   surface, with a drop shadow spilling out on every side. Firefox's
///   picture-in-picture window is one. That shadow is tens of pixels and the mat
///   is ten, so without the crop the client's own shadow — and the antialiased
///   edge of its own rounded corners — hangs outside the rounded frame, which is
///   exactly the leak this fixes.
/// - A client that has not answered the configure yet, or will not, is drawing
///   at some other size entirely. Cropping alone would show the top left corner
///   of it and call that a video, so it is scaled down to fit first, the way the
///   overview scales a window into a card. Shrink only: a client drawing smaller
///   than it was asked to is left at its own size rather than blown up soft.
///
/// **And nothing shows through it.** The opening is filled with the mat's own
/// colour behind the window, so a client that does not cover it — one still
/// starting up, one that will not take the size it was given, one with
/// transparent corners of its own, or one an edge's worth of rounding short —
/// is centred and letterboxed on more frame. What is behind a floating window
/// is the application the user is actually using, and any of it seen *inside*
/// the frame reads as the window being broken.
fn push_floating_windows<R>(
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
    let mut drawn = Vec::new();
    let space = &lxb.space;

    for window in space.elements_for_output(output).rev() {
        if !lxb.floating(window) || lxb.out_of_sight(window) {
            continue;
        }
        // Where the layout put it. A window that has started floating and has
        // not been laid out yet is left to the ordinary pass for this one frame,
        // rather than drawn against a rectangle nothing agreed to.
        let Some(frame) = crate::pip::floating_state(window).frame() else {
            continue;
        };
        let Some(geometry) = space.element_geometry(window) else {
            continue;
        };
        // How far into its arrival this window is: how much of it there is, and
        // the fraction of its own size it is drawn at. Nothing at all once it
        // has arrived, which is every frame of a video but the first handful —
        // and asking is what starts the clock, so the animation begins on the
        // frame this window is first drawn on. See [`crate::pip::arrival`].
        let (alpha, depth) = crate::pip::floating_state(window)
            .arriving(now)
            .unwrap_or((1.0, 1.0));

        // Everything below is measured from the frame the layout settled, in
        // logical coordinates relative to this output.
        let opening: Rectangle<f64, Logical> = Rectangle::new(
            (frame.inner.x, frame.inner.y).into(),
            (frame.inner.w, frame.inner.h).into(),
        );
        let corner = opening.loc;
        // The rectangle the client's own buffer is snapped to, which is what the
        // mat's opening is painted at and what stands behind the window. One
        // answer, asked once: see [`crate::pip::backing`].
        let backing = crate::pip::backing(opening, scale.x);
        // And where this window is standing on this frame, as one transform for
        // all three pieces of it: the shape the layout settled, sprung towards
        // from wherever it used to stand, and then taken to whatever fraction
        // of its own size an arrival or a departure has it at. See
        // [`Standing`], where both of those are argued for.
        let settling = crate::pip::floating_state(window).standing_in(now);
        let standing = Standing::of(
            frame.outer,
            crate::pip::deepened(settling.unwrap_or(frame.outer), depth),
            scale,
            depth < 1.0 || settling.is_some(),
        );
        // The mark that says this is the window the guide handed its directions
        // to, over the mat rather than under it: it is the *frame* that turns
        // accent, and a picture drawn behind an opaque one would only be the
        // glow around it. Nothing at all on a session where nobody has selected
        // anything, which is every session with a mouse in it.
        if let Some((selected, accent)) = lxb.outputs.pip().selected() {
            if selected == crate::overview::window_id(window) {
                // Its own breath, times however much of the window there is:
                // a mark at full strength around a window that is still arriving
                // would be the accent turning up before the video it is about.
                let breath = crate::pip::mark_alpha(lxb.start_time.elapsed()) * alpha;
                if let Some(mark) = lxb
                    .outputs
                    .pip()
                    .mark(renderer, &frame, scale.x, backing, accent, breath)
                {
                    elements.push(LxbRenderElement::Matte(standing.put(mark)));
                }
            }
        }
        if let Some(mat) = lxb
            .outputs
            .pip()
            .mat(renderer, &frame, scale.x, backing, alpha)
        {
            elements.push(LxbRenderElement::Matte(standing.put(mat)));
        }

        // Shrink to fit, and only ever shrink — but only a client that is
        // *materially* larger than its opening, because the crop below trims a
        // pixel or two for nothing and scaling a video by a ninety-ninth to save
        // them would soften every frame of it. See [`OVERDRAWS_BY`].
        let over =
            (geometry.size.w as f64 - opening.size.w).max(geometry.size.h as f64 - opening.size.h);
        let factor = match over > OVERDRAWS_BY {
            true => (opening.size.w / geometry.size.w.max(1) as f64)
                .min(opening.size.h / geometry.size.h.max(1) as f64)
                .min(1.0),
            false => 1.0,
        };
        // Centred in what is left over, which is a client that would not take
        // the size it was given: a video half the height of its opening reads as
        // letterboxed, and the same video pinned to the top of one reads as a
        // window with something wrong with it.
        let over_by = |whole: f64, part: f64| ((whole - part) / 2.0).max(0.0);
        let corner = corner
            + Point::<f64, Logical>::from((
                over_by(opening.size.w, geometry.size.w as f64 * factor),
                over_by(opening.size.h, geometry.size.h as f64 * factor),
            ));
        let anchor = corner.to_physical(scale).to_i32_round();
        // The same subtraction the ordinary window pass makes: a client drawing
        // its own decorations puts its geometry inside a larger surface, so the
        // surface starts before the geometry does — and here that spill is what
        // the crop below is for.
        let placed = crate::pip::Placed {
            frame,
            corner,
            origin: corner - window.geometry().loc.to_f64(),
            factor,
        };
        let relative = placed.origin.to_physical_precise_round(scale);
        let cut = standing.cut_to(opening, scale);
        elements.extend(
            window
                .render_elements::<WaylandSurfaceRenderElement<R>>(renderer, relative, scale, alpha)
                .into_iter()
                .filter_map(|element| {
                    CropRenderElement::from_element(
                        RescaleRenderElement::from_element(element, anchor, factor),
                        scale,
                        cut,
                    )
                })
                .map(|element| LxbRenderElement::Floating(standing.put(element))),
        );
        // And the mat's own colour behind all of it, filling the opening.
        //
        // Nothing may show through the frame. What is behind a floating window
        // is the application the user is actually using, and any of it seen
        // *inside* the frame reads as the window being broken rather than as
        // something showing through: a client that has not drawn yet, one that
        // will not take the size it was given, one whose own corners are
        // transparent, and the half pixel that rounding the opening to whole
        // ones leaves along an edge — all four looked like a hole cut in the
        // video. Painted in the mat's colour rather than in black, so what is
        // left over reads as more frame.
        elements.push(LxbRenderElement::Backing(standing.put(
            SolidColorRenderElement::new(
                crate::pip::floating_state(window).backdrop(),
                backing,
                CommitCounter::default(),
                // Premultiplied, as everything else this compositor hands a
                // renderer is: the mat's colour at this alpha is that colour
                // scaled by it.
                faded(crate::pip::BACKDROP, alpha),
                Kind::Unspecified,
            ),
        )));
        // And a note of what all that came to, so that this window can still be
        // drawn on the day its client goes away without warning — which is
        // every day, because that is how a browser closes one. See
        // [`keep_the_last_picture`].
        keep_the_last_picture(renderer, lxb, window, output, placed);
        drawn.push(crate::overview::window_id(window));
    }

    // And then the ones that have already gone, drawn from the last picture
    // taken of them. Behind the windows still floating, because a video put
    // back into a corner the moment another left it is the new one arriving
    // over the old one leaving.
    push_leaving_windows(elements, renderer, lxb, output, now, scale);
    drawn
}

/// Where a floating window is standing on this frame, as one transform.
///
/// Everything about such a window is painted for the rectangle the *layout*
/// settled — the mat at that size, the client configured to that opening, the
/// colour behind it filling that opening — and then all three are moved
/// together onto wherever the window actually is at this moment. Three things,
/// one transform, one origin: a mat three pixels thick has nothing to spare for
/// three separate pieces of arithmetic agreeing about where a corner is.
///
/// Two animations end up in here, and they compose because both of them are
/// only ever a rectangle:
///
/// - **Arriving and leaving** ([`crate::pip::arrival`]), which is the shape at
///   a fraction of its own size about its own middle.
/// - **Settling** ([`crate::pip::settling`]), which is the shape springing from
///   where it used to stand to where the layout has just put it — the column
///   closing up behind a window pulled out of it, or the whole column changing
///   size because somebody moved a slider on the Settings page. The spring goes
///   *past* its destination and comes back, so the rectangle this is asked
///   about is regularly outside both ends of the journey.
///
/// Never repainted, always transformed. Painting a mat is a few hundred
/// thousand distance fields and they are cached by size, so a window whose
/// shape was re-derived every frame of a spring would repaint one on every
/// frame of it — which on a 4K panel is a tenth of a second of processor, per
/// frame, for half a second.
#[derive(Debug, Clone, Copy)]
struct Standing {
    /// The corner of the shape everything is painted for, which is what the
    /// scale below is taken about.
    origin: Point<i32, Physical>,
    /// How much larger or smaller than that shape this frame's is — per axis,
    /// because a spring from one shape to another of a different proportion is
    /// exactly the squash that makes it read as something soft.
    scale: Scale<f64>,
    /// And how far the whole thing has moved.
    shift: Point<i32, Physical>,
    /// Whether this is anything at all, which it is not on any frame of a video
    /// simply sitting in its corner.
    moved: bool,
}

impl Standing {
    /// The transform that takes `target` — the shape everything is painted for
    /// — onto `visible`, the shape it is to appear in this frame.
    fn of(
        target: lxb_protocol::overview::Rect,
        visible: lxb_protocol::overview::Rect,
        scale: Scale<f64>,
        moved: bool,
    ) -> Self {
        let corner = |rect: &lxb_protocol::overview::Rect| {
            Point::<f64, Logical>::from((rect.x, rect.y))
                .to_physical(scale)
                .to_i32_round()
        };
        let origin = corner(&target);
        Self {
            origin,
            scale: Scale::from((
                visible.w / target.w.max(f64::EPSILON),
                visible.h / target.h.max(f64::EPSILON),
            )),
            shift: corner(&visible) - origin,
            moved,
        }
    }

    /// One piece of the window, put where the window is.
    fn put<E: Element>(self, element: E) -> RelocateRenderElement<RescaleRenderElement<E>> {
        RelocateRenderElement::from_element(
            RescaleRenderElement::from_element(element, self.origin, self.scale),
            self.shift,
            Relocate::Relative,
        )
    }

    /// The rectangle a floating window's own drawing is cut to.
    ///
    /// Its opening exactly while the window is standing still, and one physical
    /// pixel inside it while it is being moved.
    ///
    /// That pixel is the price of the transform. Scaling rounds a rectangle's
    /// corner and its size to whole pixels separately, so two rectangles that
    /// shared an edge before can be a pixel apart afterwards, and the two here
    /// are the mat's painted opening and the video inside it. A pixel of *mat*
    /// over the video is nothing — the mat lies on the edge of the picture
    /// already, which is [`crate::pip::OVERLAP`]. A pixel of *video* outside the
    /// mat is a square corner on a rounded window. So the video gives way, for
    /// as long as the window is moving, and what shows in its place is the mat's
    /// own colour behind it.
    fn cut_to(
        self,
        opening: Rectangle<f64, Logical>,
        scale: Scale<f64>,
    ) -> Rectangle<i32, Physical> {
        let mut cut: Rectangle<i32, Physical> = opening.to_physical_precise_round(scale);
        if self.moved {
            cut.loc += Point::from((1, 1));
            cut.size.w = (cut.size.w - 2).max(0);
            cut.size.h = (cut.size.h - 2).max(0);
        }
        cut
    }
}

/// A premultiplied colour with `alpha` of it left.
fn faded(colour: [f32; 4], alpha: f32) -> [f32; 4] {
    [
        colour[0] * alpha,
        colour[1] * alpha,
        colour[2] * alpha,
        colour[3] * alpha,
    ]
}

/// Take a note of what a floating window looks like on this frame: the picture
/// itself, and where on the display it was put.
///
/// One texture handle cloned per surface of the window — a reference count, not
/// a copy of anything — and nothing at all on a session with no video parked in
/// a corner. What it buys is a window that can be faded *out*, which nothing
/// else here could give: a browser does not stop calling its window
/// picture-in-picture when the video goes back into the page, it destroys the
/// window, and a destroyed surface has no picture to fade. See
/// [`crate::pip::LastPicture`], where the whole of that argument lives.
///
/// The tree is walked exactly as smithay walks it to build the elements above —
/// same offsets, same order — because what is kept has to land where the live
/// window was standing. The offsets are kept in logical coordinates so that the
/// same note can be drawn at any scale: this is also asked while the small
/// picture behind the shell's glass is being built, which is the same frame at
/// a different size.
fn keep_the_last_picture<R>(
    renderer: &R,
    lxb: &Lxb,
    window: &Window,
    output: &Output,
    placed: crate::pip::Placed,
) where
    R: Renderer,
    R::TextureId: Send + Clone + 'static,
{
    let Some(surface) = window.wl_surface() else {
        return;
    };
    let mut picture = crate::pip::KeptPicture::new(renderer.context_id());
    with_surface_tree_downward(
        &surface,
        Point::<i32, Logical>::default(),
        |_, states, offset| {
            let Some(data) = states.data_map.get::<RendererSurfaceStateUserData>() else {
                return TraversalAction::SkipChildren;
            };
            match data.lock().unwrap().view() {
                Some(view) => TraversalAction::DoChildren(*offset + view.offset),
                None => TraversalAction::SkipChildren,
            }
        },
        |surface, states, offset| {
            let Some(data) = states.data_map.get::<RendererSurfaceStateUserData>() else {
                return;
            };
            let data = data.lock().unwrap();
            let Some(view) = data.view() else {
                return;
            };
            picture.keep(
                Id::from_wayland_resource(surface),
                *offset + view.offset,
                &data,
            );
        },
        |_, _, _| true,
    );
    if picture.is_empty() {
        return;
    }
    lxb.outputs.pip().keep(
        crate::overview::window_id(window),
        crate::pip::LastPicture::new(
            output.name(),
            placed,
            crate::pip::floating_state(window).backdrop(),
            picture,
        ),
    );
}

/// Draw every floating window that has gone, fading and falling back out of the
/// corner it was in.
///
/// Nothing of the window itself is left by now — see [`keep_the_last_picture`]
/// for why — so all three parts of it come from somewhere else. The picture is
/// the note taken on the last frame it was drawn; the mat is painted for the
/// shape it was in, which is the very same picture the live window was using
/// and so is still in hand; and the colour behind it is a rectangle.
fn push_leaving_windows<R>(
    elements: &mut Vec<LxbRenderElement<R>>,
    renderer: &mut R,
    lxb: &Lxb,
    output: &Output,
    now: std::time::Instant,
    scale: Scale<f64>,
) where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Send + Clone + 'static,
{
    if !lxb.outputs.pip().anything_going() {
        return;
    }
    let name = output.name();
    for (kept, alpha, depth) in lxb.outputs.pip().going_on(&name, now) {
        let placed = kept.placed();
        let opening: Rectangle<f64, Logical> = Rectangle::new(
            (placed.frame.inner.x, placed.frame.inner.y).into(),
            (placed.frame.inner.w, placed.frame.inner.h).into(),
        );
        let backing = crate::pip::backing(opening, scale.x);
        // No spring here: a window on its way out has nowhere left to be laid
        // out to. Only the fall back out of the screen.
        let standing = Standing::of(
            placed.frame.outer,
            crate::pip::deepened(placed.frame.outer, depth),
            scale,
            true,
        );
        if let Some(mat) = lxb
            .outputs
            .pip()
            .mat(renderer, &placed.frame, scale.x, backing, alpha)
        {
            elements.push(LxbRenderElement::Matte(standing.put(mat)));
        }
        // The picture, if it was this renderer that took it. One that does not
        // recognise the note — a second GPU on a session that drives two — draws
        // the frame fading out with nothing inside it, which is a worse fade
        // than this one and a better one than none.
        if let Some(picture) = kept.picture::<R::TextureId>() {
            let anchor = placed.corner.to_physical(scale).to_i32_round();
            let cut = standing.cut_to(opening, scale);
            elements.extend(
                picture
                    .elements(
                        renderer.context_id(),
                        placed.origin.to_physical(scale),
                        scale,
                        alpha,
                    )
                    .into_iter()
                    .filter_map(|element| {
                        CropRenderElement::from_element(
                            RescaleRenderElement::from_element(element, anchor, placed.factor),
                            scale,
                            cut,
                        )
                    })
                    .map(|element| LxbRenderElement::Kept(standing.put(element))),
            );
        }
        elements.push(LxbRenderElement::Backing(standing.put(
            SolidColorRenderElement::new(
                kept.backdrop(),
                backing,
                CommitCounter::default(),
                faded(crate::pip::BACKDROP, alpha),
                Kind::Unspecified,
            ),
        )));
    }
}

/// Everything the compositor draws on one side of the shell's own surfaces.
///
/// What a pane of the shell's glass has to be able to see and cannot: another
/// client. The shell reads back what it drew itself and evaluates its own
/// wallpaper, so those two it has; a game, or the video in a floating window,
/// it can do neither with — see [`crate::capture::behind`], which draws this
/// small and hands it over.
///
/// Front to back, as the display's own list is, and made of the same builders:
/// what is in this picture is what the display is drawing there, at another
/// size. The shell's own surfaces are not in it in either direction — it has
/// those already, at full resolution, and drawing them here would be handing
/// the shell a blurred copy of its own frame.
pub fn elements_behind_the_shell<R>(
    renderer: &mut R,
    lxb: &Lxb,
    output: &Output,
    side: crate::capture::Side,
    scale: Scale<f64>,
) -> Vec<LxbRenderElement<R>>
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Send + Clone + 'static,
{
    let now = std::time::Instant::now();
    // What is in front of the shell's session surface and behind its menu: the
    // floating windows, and anything flying back out of a tile. Built either
    // way round, because the pass behind needs to know what these two already
    // took — which is the same reason the display's own frame draws them first.
    // The menu is deliberately in neither: a pane on it cannot refract itself.
    let mut ahead = Vec::new();
    let floating = push_floating_windows(&mut ahead, renderer, lxb, output, now, scale);
    let flying = push_restoring_windows(&mut ahead, renderer, lxb, output, now, scale);
    if side == crate::capture::Side::Above {
        return ahead;
    }

    let mut elements = Vec::new();
    push_windows(
        &mut elements,
        renderer,
        lxb,
        output,
        now,
        scale,
        &flying,
        &floating,
    );
    elements
}

/// The application windows on `output`, topmost first — either where they
/// really are, or, in the overview, somewhere between there and their card.
///
/// Its own function because it is asked for twice: once for the display, and
/// once at a fraction of the size for the picture a pane of the shell's glass
/// refracts. See [`crate::capture::behind`], which is why every builder here
/// takes the scale to draw at rather than reading the output's own.
///
/// `flying` and `floating` are what has already been drawn in front of the
/// shell and must not be drawn again here.
#[allow(clippy::too_many_arguments)]
fn push_windows<R>(
    elements: &mut Vec<LxbRenderElement<R>>,
    renderer: &mut R,
    lxb: &Lxb,
    output: &Output,
    now: std::time::Instant,
    scale: Scale<f64>,
    flying: &[u32],
    floating: &[u32],
) where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Send + Clone + 'static,
{
    let space = &lxb.space;
    let Some(output_geo) = space.output_geometry(output) else {
        return;
    };
    let overview = lxb.overview.progress(output, now);
    if overview > 0.0 {
        push_overview_windows(elements, renderer, lxb, output, overview, scale);
        return;
    }
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
        // Already drawn, in its corner, over everything above.
        if floating.contains(&crate::overview::window_id(window)) {
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
        let relative =
            (location - output_geo.loc - window.geometry().loc).to_physical_precise_round(scale);
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
        let factor = lxb.outputs.window_scale_on(window, output);
        if factor == 1.0 {
            elements.extend(surfaces.into_iter().map(LxbRenderElement::Surface));
            continue;
        }
        let anchor = (location - output_geo.loc).to_physical_precise_round(scale);
        elements.extend(surfaces.into_iter().map(|element| {
            LxbRenderElement::Scaled(RescaleRenderElement::from_element(element, anchor, factor))
        }));
    }
}

/// Draw the shell's context menu in front of the floating windows.
///
/// **The one exception to "a floating window is over everything".** A window the
/// user has made large enough covers the very menu that offers to make it small
/// again, and on a session driven by a controller that is a dead end: there is
/// no pointer to find an unseen row with, so a menu that cannot be seen cannot
/// be answered. One surface is therefore allowed through, and it is the one the
/// shell draws its menu on and nothing else.
///
/// **A surface of its own, and not a rectangle of the shell's main one**, which
/// is not the obvious choice and is the one that survived contact. A panel is
/// rounded and a rectangle is not, so a crop to its bounding box lifts four
/// square corners of whatever else the shell was drawing and lays them over the
/// video — the video being what should be showing there. Given its own surface
/// the panel is exactly its own shape, whatever that shape is.
///
/// It is drawn here and *not* in its parent's pass, which is what
/// [`elements_of_all_but_the_menu`] is for: this is a move, not a copy.
fn push_the_menu_over_floating_windows<R>(
    elements: &mut Vec<LxbRenderElement<R>>,
    renderer: &mut R,
    lxb: &Lxb,
    output: &Output,
    scale: Scale<f64>,
) where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Send + Clone + 'static,
{
    if !lxb.outputs.pip().has_a_menu() {
        return;
    }
    let layer_map = layer_map_for_output(output);
    for layer in [Layer::Overlay, Layer::Top, Layer::Bottom, Layer::Background] {
        for surface in layer_map.layers_on(layer).rev() {
            let Some(geometry) = layer_map.layer_geometry(surface) else {
                continue;
            };
            let location = geometry.loc.to_physical_precise_round(scale);
            if let Some(menu) = menu_under(lxb, surface.wl_surface()) {
                // Where the parent is, because that is where the shell puts it:
                // a subsurface of the whole display, offset by nothing. A shell
                // that moved it would have to say so, and none does.
                elements.extend(
                    render_elements_from_surface_tree::<R, WaylandSurfaceRenderElement<R>>(
                        renderer,
                        &menu,
                        location,
                        scale,
                        1.0,
                        Kind::Unspecified,
                    )
                    .into_iter()
                    .map(LxbRenderElement::Surface),
                );
            }
        }
    }
}

/// The menu surface inside one layer surface's tree, if it is in this one.
///
/// Asked of every layer surface on the display rather than answered from the
/// shell's word alone, because the shell names a *surface* and never says which
/// display it is on — which display a surface belongs to is a question the
/// layer map already answers, and answering it twice is how the two come to
/// disagree.
fn menu_under(lxb: &Lxb, root: &WlSurface) -> Option<WlSurface> {
    let mut found = None;
    with_surface_tree_downward(
        root,
        (),
        |surface, _, ()| match lxb.outputs.pip().is_a_menu(surface) {
            true => TraversalAction::SkipChildren,
            false => TraversalAction::DoChildren(()),
        },
        |surface, _, ()| {
            if lxb.outputs.pip().is_a_menu(surface) {
                found = Some(surface.clone());
            }
        },
        |_, _, ()| true,
    );
    found
}

/// One layer surface's render elements, with the context menu in it left out.
///
/// Smithay's own [`render_elements_from_surface_tree`] walks the whole tree and
/// has no opinion about any of it, which is right for every other surface on the
/// session and wrong for this one: the menu inside it is drawn somewhere else
/// entirely — see [`push_the_menu_over_floating_windows`] — and a tree walked
/// whole would draw it twice.
///
/// A transcription of that function with one thing added: a subtree the caller
/// names is skipped, root and children alike. The rest of it is smithay's, down
/// to the order the offsets accumulate in, because the two have to put every
/// other surface in exactly the same place.
fn elements_of_all_but_the_menu<R>(
    renderer: &mut R,
    lxb: &Lxb,
    root: &WlSurface,
    location: Point<i32, Physical>,
    scale: Scale<f64>,
) -> Vec<WaylandSurfaceRenderElement<R>>
where
    R: Renderer + ImportAll,
    R::TextureId: Clone + 'static,
{
    let mut surfaces = Vec::new();
    with_surface_tree_downward(
        root,
        location.to_f64(),
        |surface, states, location| {
            // The whole subtree, skipped where it begins. `SkipChildren` still
            // offers the surface itself to the processor below, which is where
            // it is turned away.
            if lxb.outputs.pip().is_a_menu(surface) {
                return TraversalAction::SkipChildren;
            }
            let mut location = *location;
            let Some(data) = states.data_map.get::<RendererSurfaceStateUserData>() else {
                return TraversalAction::SkipChildren;
            };
            match data.lock().unwrap().view() {
                Some(view) => {
                    location += view.offset.to_f64().to_physical(scale);
                    TraversalAction::DoChildren(location)
                }
                None => TraversalAction::SkipChildren,
            }
        },
        |surface, states, location| {
            if lxb.outputs.pip().is_a_menu(surface) {
                return;
            }
            let mut location = *location;
            let Some(data) = states.data_map.get::<RendererSurfaceStateUserData>() else {
                return;
            };
            let has_view = match data.lock().unwrap().view() {
                Some(view) => {
                    location += view.offset.to_f64().to_physical(scale);
                    true
                }
                None => false,
            };
            if !has_view {
                return;
            }
            match WaylandSurfaceRenderElement::from_surface(
                renderer,
                surface,
                states,
                location,
                1.0,
                Kind::Unspecified,
            ) {
                Ok(Some(element)) => surfaces.push(element),
                Ok(None) => {}
                Err(err) => tracing::warn!(?err, "could not import a surface of the shell's"),
            }
        },
        |_, _, _| true,
    );
    surfaces
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
        if lxb.floating(window) {
            continue;
        }
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
        let factor = lxb.outputs.window_scale_on(window, output);
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
        let factor = lxb.outputs.window_scale_on(window, output);
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
/// Three answers are the whole display at once. While the shell is painting
/// over it nothing of anybody's is on screen — its own windows included, since
/// there is no application in front for them to belong to. While the display
/// is resting all the way behind OLED protection's black nothing is either, the
/// floating window included, since the sheet is over that too: what is under
/// it sleeps until input brings the display back. See [`crate::blackout`]. And
/// while the overview is up everything is, because those cards are the live
/// windows themselves.
///
/// Except a window that has never been on screen *at all*, which is on every
/// list until it has been on one. See [`has_been_seen`]: that is the window a
/// loading screen is waiting for, and it is the one thing under a covered
/// display that must go on running. On a resting display it is also the one
/// thing that may wake it: what it paints is what `output_drawing` reports, and
/// a display with something new painting on it is not left resting.
///
/// A window the shell is driving out of sight is on no list. It is not on the
/// screen, which is the question this answers; that it must still be sent
/// frames is [`post_repaint`]'s own business, and that it must never be put to
/// sleep is [`crate::sleep`]'s.
pub fn windows_on_screen(lxb: &Lxb, output: &Output) -> Vec<Window> {
    // Read off the flight rather than a flag, so windows are already running
    // by the time they arrive in their cards and keep running until the last
    // one has flown home.
    let now = std::time::Instant::now();
    let overview_up = lxb.overview.progress(output, now) > 0.0;
    if !overview_up && lxb.blackouts.is_black(output, now) {
        // Resting all the way behind the black, which is over everything on
        // this display — the floating window too — so only the newcomers.
        return arriving_on(lxb, output, &[]);
    }
    if !overview_up && shell_hides_the_display(lxb, output) {
        // Except the floating window, which the shell is not painting over: it
        // is drawn in front of the shell's own surfaces. Saying otherwise here
        // would stop the video the moment the start screen came up — this list
        // is also what [`crate::sleep`] reads to decide what may be put to
        // sleep — which is the one thing a window that floats over everything
        // must never do.
        let mut shown = floating_windows(lxb, output);
        let arriving = arriving_on(lxb, output, &shown);
        shown.extend(arriving);
        return shown;
    }
    let front = (!overview_up)
        .then(|| front_application(lxb, output))
        .flatten();
    let cover = front.as_ref().and_then(|window| on_screen(lxb, window));

    let mut shown = Vec::new();
    let mut behind_front = false;
    for window in lxb.space.elements_for_output(output).rev() {
        // Marked before anything is decided about it, and for every window
        // rather than for the ones this answers yes about: the shell is not
        // over this display, so a window behind the application in front has
        // had its chance to be seen and lost it, which is an ordinary covered
        // window and not one still arriving. See [`has_been_seen`].
        mark_as_seen(window);
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

/// The newcomers on a display that is covered whole — by the shell's own start
/// screen, or by OLED protection's black — and so is showing nothing of
/// anybody's: the other thing such a display can have on it that nobody is
/// hiding from anybody. See [`has_been_seen`]. `besides` is what is already on
/// the list, so nothing is on it twice.
///
/// Nothing is marked here: a covered display is precisely where a window does
/// not get its chance.
fn arriving_on(lxb: &Lxb, output: &Output, besides: &[Window]) -> Vec<Window> {
    lxb.space
        .elements_for_output(output)
        .rev()
        .filter(|window| !lxb.out_of_sight(window))
        .filter(|window| !has_been_seen(window))
        .filter(|window| !besides.contains(window))
        .cloned()
        .collect()
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
        .find(|window| {
            window_accepts_keyboard_focus(window)
                && !lxb.out_of_sight(window)
                // The floating window is topmost and is never the front
                // application. It is a corner of the screen, not the thing the
                // user is doing — and calling it the front would take the
                // frames away from the game behind it and hand them to a video.
                && !lxb.floating(window)
        })
        .cloned()
}

/// The floating windows on `output`, which are on screen whatever else is.
fn floating_windows(lxb: &Lxb, output: &Output) -> Vec<Window> {
    lxb.space
        .elements_for_output(output)
        .rev()
        .filter(|window| lxb.floating(window) && !lxb.out_of_sight(window))
        .cloned()
        .collect()
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
///
/// **The shell asks the same question and must get the same answer**, which is
/// why this is not private to the render pass. It has its own reason to know
/// whether an application covers its bar — it stops drawing when one does — and
/// it can only be told, so `lxb_shell_v1.output_window` carries this rectangle's
/// size. Sending the configured size instead meant the shell's answer and this
/// one disagreed by exactly the scale factor, and a session with larger
/// applications had its start screen drawn behind every Wayland one of them for
/// as long as it was in front. See [`crate::shell_control`].
pub(crate) fn on_screen(lxb: &Lxb, window: &Window) -> Option<Rectangle<i32, Logical>> {
    let geometry = lxb.space.element_geometry(window)?;
    let factor = lxb.outputs.window_scale(&lxb.space, window);
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

/// Whether this window has ever been on a display the shell was not standing
/// over.
///
/// The one thing under a covered display that is not hidden from anybody. A
/// window that has never been anywhere has not been *covered*: nobody has had
/// the chance to see it and fail to, and something is still deciding what to do
/// with it. That something is usually a loading screen — the window it is
/// waiting for maps underneath it and is revealed rather than switched to — and
/// a window starved of its frames and stopped in that gap is an application
/// that never finishes opening. It is not only the shell's own launches: a game
/// Valve's client is starting arrives the same way, under the same splash, and
/// the shell has no pid for it to name.
///
/// The mark goes on in [`windows_on_screen`] for every window on a display the
/// shell is not over, whether or not that window is one of the ones on screen —
/// a window behind the application in front has had its chance and lost it,
/// which is an ordinary covered window. So this means exactly *this window
/// arrived while the shell was standing over the display and the shell has not
/// lifted since*, and it stops meaning it by itself the moment the shell does.
///
/// Kept on the window rather than in a table beside it, as [`LastDrawn`] is, so
/// that it goes when the window does.
#[derive(Debug, Default)]
struct BeenSeen(std::cell::Cell<bool>);

fn has_been_seen(window: &Window) -> bool {
    window
        .user_data()
        .get::<BeenSeen>()
        .is_some_and(|seen| seen.0.get())
}

fn mark_as_seen(window: &Window) {
    window.user_data().insert_if_missing(BeenSeen::default);
    if let Some(seen) = window.user_data().get::<BeenSeen>() {
        seen.0.set(true);
    }
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
    use super::{covered, region_hides, Standing};
    use lxb_protocol::overview::Rect;
    use smithay::utils::{Logical, Physical, Point, Rectangle, Scale};

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Logical> {
        Rectangle::new((x, y).into(), (w, h).into())
    }

    fn a_frame() -> lxb_protocol::pip::Frame {
        lxb_protocol::pip::Frame {
            outer: Rect {
                x: 1400.0,
                y: 760.0,
                w: 480.0,
                h: 280.0,
            },
            inner: Rect {
                x: 1403.0,
                y: 763.0,
                w: 474.0,
                h: 274.0,
            },
            radius: 12.0,
            border: 3.0,
            shadow: 24.0,
        }
    }

    /// Where a corner of the shape everything is painted for lands once this
    /// transform has been applied to it — the same arithmetic smithay's own two
    /// wrappers do, in one place so a test can ask about the result.
    fn lands(
        standing: Standing,
        at: Point<f64, Logical>,
        scale: Scale<f64>,
    ) -> Point<f64, Physical> {
        let at = at.to_physical(scale);
        let origin = standing.origin.to_f64();
        Point::from((
            origin.x + (at.x - origin.x) * standing.scale.x + standing.shift.x as f64,
            origin.y + (at.y - origin.y) * standing.scale.y + standing.shift.y as f64,
        ))
    }

    /// The one thing this transform has to do: take the shape everything is
    /// painted for onto the shape the window is standing in.
    ///
    /// Asked of both corners, because a scale that is right about one corner and
    /// wrong about the other is a window of the wrong size — and asked of a
    /// spring that has overshot, which is a rectangle *outside* both ends of the
    /// journey and the case the arithmetic is easiest to get wrong on.
    #[test]
    fn a_floating_window_lands_on_the_shape_it_is_standing_in() {
        let target = a_frame().outer;
        let cases = [
            ("at rest", target),
            ("behind the screen", crate::pip::deepened(target, 0.88)),
            (
                "sprung past a smaller shape",
                Rect {
                    x: 1180.0,
                    y: 700.0,
                    w: 620.0,
                    h: 361.0,
                },
            ),
            (
                "sprung past a larger one",
                Rect {
                    x: 1470.0,
                    y: 800.0,
                    w: 360.0,
                    h: 210.0,
                },
            ),
        ];
        for scale in [1.0, 1.5, 2.0] {
            let scale = Scale::from(scale);
            for (name, visible) in cases {
                let standing = Standing::of(target, visible, scale, visible != target);
                for (corner, expected) in [
                    ((target.x, target.y), (visible.x, visible.y)),
                    (
                        (target.x + target.w, target.y + target.h),
                        (visible.x + visible.w, visible.y + visible.h),
                    ),
                ] {
                    let landed = lands(standing, Point::from(corner), scale);
                    let want = Point::<f64, Logical>::from(expected).to_physical(scale);
                    assert!(
                        (landed.x - want.x).abs() <= 1.0 && (landed.y - want.y).abs() <= 1.0,
                        "{name} at {scale:?}: {landed:?} should be {want:?}"
                    );
                }
            }
        }
    }

    /// A window standing still is not transformed at all — no scale, no shift,
    /// and its own drawing cut to its opening and no less.
    #[test]
    fn a_window_standing_still_is_left_exactly_where_it_is() {
        let target = a_frame().outer;
        let opening: Rectangle<f64, Logical> =
            Rectangle::new((1403.0, 763.0).into(), (474.0, 274.0).into());
        for scale in [1.0, 1.5, 2.0] {
            let scale = Scale::from(scale);
            let standing = Standing::of(target, target, scale, false);
            assert_eq!(standing.scale, Scale::from(1.0));
            assert_eq!(standing.shift, Point::from((0, 0)));
            assert_eq!(
                standing.cut_to(opening, scale),
                opening.to_physical_precise_round(scale),
                "a window standing still is cut to its opening and no less"
            );
        }
    }

    /// And one that is moving gives the mat a pixel to round it with — see
    /// [`Standing::cut_to`], where that pixel is argued for.
    #[test]
    fn a_window_being_moved_gives_the_mat_a_pixel_to_round_it_with() {
        let target = a_frame().outer;
        let opening: Rectangle<f64, Logical> =
            Rectangle::new((1403.0, 763.0).into(), (474.0, 274.0).into());
        for scale in [1.0, 1.5, 2.0] {
            let scale = Scale::from(scale);
            let whole: Rectangle<i32, Physical> = opening.to_physical_precise_round(scale);
            let cut = Standing::of(target, target, scale, true).cut_to(opening, scale);
            assert!(
                whole.contains_rect(cut),
                "a window being moved is cut inside its opening: {cut:?} in {whole:?}"
            );
            assert_eq!(cut.loc - whole.loc, Point::from((1, 1)));
            assert_eq!(whole.size.w - cut.size.w, 2);
            assert_eq!(whole.size.h - cut.size.h, 2);
        }
    }

    /// And an opening too small to give a pixel away gives none: a rectangle of
    /// negative size is not a smaller rectangle.
    #[test]
    fn an_opening_with_no_pixel_to_spare_is_not_cut_to_nothing() {
        let target = a_frame().outer;
        for edge in [0.0, 1.0, 2.0] {
            let opening: Rectangle<f64, Logical> =
                Rectangle::new((0.0, 0.0).into(), (edge, edge).into());
            let cut = Standing::of(target, target, Scale::from(1.0), true)
                .cut_to(opening, Scale::from(1.0));
            assert!(cut.size.w >= 0 && cut.size.h >= 0, "{edge}: {cut:?}");
        }
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
