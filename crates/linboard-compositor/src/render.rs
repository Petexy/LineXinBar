//! Render element assembly, shared by every backend.
//!
//! Both the nested and the DRM backend need the exact same element list for a
//! given output, so it is built once here and is generic over the renderer.

use smithay::backend::renderer::element::memory::MemoryRenderBufferRenderElement;
use smithay::backend::renderer::element::surface::WaylandSurfaceRenderElement;
use smithay::backend::renderer::element::utils::RescaleRenderElement;
use smithay::backend::renderer::element::AsRenderElements;
use smithay::backend::renderer::{ImportAll, ImportMem, Renderer};
use smithay::desktop::{layer_map_for_output, Space, Window};
use smithay::output::Output;
use smithay::utils::{Rectangle, Scale};
use smithay::wayland::shell::wlr_layer::Layer;

use crate::input::window_accepts_keyboard_focus;
use crate::state::Linboard;

pub use crate::cursor::CursorState;

smithay::render_elements! {
    /// Everything Linboard can put on screen.
    pub LinboardRenderElement<R> where R: ImportAll + ImportMem;
    /// A client surface (window, layer surface, popup or cursor surface).
    Surface = WaylandSurfaceRenderElement<R>,
    /// A window mid-flight in the overview, drawn scaled into its card.
    OverviewCard = RescaleRenderElement<WaylandSurfaceRenderElement<R>>,
    /// A CPU-side image: the themed cursor.
    Memory = MemoryRenderBufferRenderElement<R>,
}

/// The windows the overview shows for `output`, topmost first — the same
/// order they are announced to the shell in, so slot N here is the card the
/// shell is framing as window N.
pub fn overview_windows(space: &Space<Window>, output: &Output) -> Vec<Window> {
    space
        .elements_for_output(output)
        .rev()
        .filter(|window| window_accepts_keyboard_focus(window))
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
    linboard: &Linboard,
    output: &Output,
    cursor: Option<&mut CursorState>,
) -> Vec<LinboardRenderElement<R>>
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Send + Clone + 'static,
{
    let space = &linboard.space;
    let now = std::time::Instant::now();
    let scale = Scale::from(output.current_scale().fractional_scale());
    let Some(output_geo) = space.output_geometry(output) else {
        return Vec::new();
    };

    let mut elements: Vec<LinboardRenderElement<R>> = Vec::new();

    // Cursor first: elements are drawn front to back.
    if let Some(cursor) = cursor {
        let position = (linboard.pointer_location - output_geo.loc.to_f64())
            .to_physical(scale)
            .to_i32_round();
        elements.extend(cursor.render(renderer, position, scale, linboard.start_time.elapsed()));
    }

    let layer_map = layer_map_for_output(output);

    let push_layer =
        |elements: &mut Vec<LinboardRenderElement<R>>, layer: Layer, renderer: &mut R| {
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
                        .map(LinboardRenderElement::Surface),
                );
            }
        };

    push_layer(&mut elements, Layer::Overlay, renderer);
    push_layer(&mut elements, Layer::Top, renderer);

    // Windows, topmost first — either where they really are, or (in the
    // overview) somewhere between there and their card.
    let overview = linboard.overview.progress(output, now);
    if overview > 0.0 {
        push_overview_windows(&mut elements, renderer, linboard, output, overview, scale);
    } else {
        for window in space.elements_for_output(output).rev() {
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
                    .map(LinboardRenderElement::Surface),
            );
        }
    }

    push_layer(&mut elements, Layer::Bottom, renderer);
    push_layer(&mut elements, Layer::Background, renderer);

    elements
}

/// Draw the overview's windows, each `progress` of the way between its real
/// geometry and its aspect-fitted card.
///
/// The card slots come from the layout shared with the shell, so the frames
/// and titles the shell paints on its overlay land exactly on the windows
/// drawn here.
fn push_overview_windows<R>(
    elements: &mut Vec<LinboardRenderElement<R>>,
    renderer: &mut R,
    linboard: &Linboard,
    output: &Output,
    progress: f64,
    scale: Scale<f64>,
) where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Send + Clone + 'static,
{
    let space = &linboard.space;
    let Some(output_geo) = space.output_geometry(output) else {
        return;
    };

    let windows = overview_windows(space, output);
    // One extra card: the trailing start-screen card the shell appends. It
    // occupies a slot (so counting and scrolling agree with the shell) but
    // no window is drawn there — the shell paints it.
    let count = windows.len() + 1;
    let selected = linboard.overview.selection(output);
    let slots = linboard_protocol::overview::card_slots(
        output_geo.size.w as f64,
        output_geo.size.h as f64,
        count,
        selected,
    );

    let dt = linboard.overview.tick(output, std::time::Instant::now());

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
            linboard_protocol::overview::fit(slot, geometry.size.w as f64, geometry.size.h as f64);
        // The slot end of the flight eases towards its place, which is what
        // turns a scroll of the row into a glide instead of a teleport.
        let seat = linboard
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
                    LinboardRenderElement::OverviewCard(RescaleRenderElement::from_element(
                        element,
                        anchor,
                        window_scale,
                    ))
                }),
        );
    }
}

/// Release frame callbacks for everything shown on `output`.
///
/// Clients block on these before drawing their next frame, so a backend that
/// forgets to call this will appear to hang every client on that output.
pub fn post_repaint(
    space: &Space<Window>,
    output: &Output,
    time: std::time::Duration,
    throttle: Option<std::time::Duration>,
) {
    for window in space.elements_for_output(output) {
        window.send_frame(output, time, throttle, |_, _| Some(output.clone()));
    }

    let map = layer_map_for_output(output);
    for layer in map.layers() {
        layer.send_frame(output, time, throttle, |_, _| Some(output.clone()));
    }
}
