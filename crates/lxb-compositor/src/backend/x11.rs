//! Nested backend with one window per virtual output.
//!
//! The winit backend can only ever open one window, which makes it awkward to
//! exercise the multi-display code paths without owning several monitors. The
//! X11 backend has no such limit: `--outputs 3` opens three windows, each a
//! fully independent [`Output`] with its own damage tracking and its own place
//! in the logical layout, so hotplug-free multi-display debugging works from
//! inside a normal desktop session (via Xwayland, if you are on Wayland).

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use smithay::backend::allocator::dmabuf::{Dmabuf, DmabufAllocator};
use smithay::backend::allocator::gbm::{GbmAllocator, GbmBufferFlags, GbmDevice};
use smithay::backend::egl::{EGLContext, EGLDisplay};
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::{Bind, ImportDma};
use smithay::backend::x11::{Window, WindowBuilder, X11Event, X11Surface};
use smithay::output::{Mode, Output, PhysicalProperties, Subpixel};
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_server::backend::GlobalId;
use smithay::reexports::wayland_server::Display;
use smithay::utils::{DeviceFd, Transform};
use smithay::wayland::dmabuf::{DmabufFeedbackBuilder, DmabufGlobal};

use super::ImportError;
use crate::config::Config;
use crate::render::{output_elements, post_repaint, CursorState};
use crate::state::LxbState;

/// One nested window acting as a virtual display.
struct VirtualOutput {
    /// Owns the X11 window; dropping it would unmap the virtual display.
    #[allow(dead_code)]
    window: Window,
    surface: X11Surface,
    output: Output,
    damage_tracker: OutputDamageTracker,
    _global: GlobalId,
    full_redraw: bool,
}

pub struct X11Backend {
    renderer: GlesRenderer,
    /// Keyed by X11 window id, which is how the backend tags its events.
    outputs: HashMap<u32, VirtualOutput>,
    /// Host windows currently holding X11 focus (normally zero or one).
    focused_windows: HashSet<u32>,
    cursor: CursorState,
    dmabuf_global: Option<DmabufGlobal>,
}

impl X11Backend {
    pub fn import_dmabuf(&mut self, dmabuf: &Dmabuf) -> Result<(), ImportError> {
        self.renderer
            .import_dmabuf(dmabuf, None)
            .map(|_| ())
            .map_err(|e| ImportError::Failed(e.to_string()))
    }

    pub fn capture_window(
        &mut self,
        window: &smithay::desktop::Window,
        scale: f64,
    ) -> anyhow::Result<crate::capture::Shot> {
        crate::capture::window(&mut self.renderer, window, scale)
    }

    pub fn capture_output(
        &mut self,
        lxb: &crate::state::Lxb,
        output: &Output,
    ) -> anyhow::Result<crate::capture::Shot> {
        crate::capture::output(&mut self.renderer, lxb, output)
    }
}

/// Bring up the compositor with `count` nested windows.
pub fn init(
    event_loop: &mut EventLoop<'static, LxbState>,
    display: Display<LxbState>,
    config: Config,
    socket_name: Option<String>,
    count: usize,
    size: (i32, i32),
) -> anyhow::Result<LxbState> {
    let backend = smithay::backend::x11::X11Backend::new()
        .map_err(|e| anyhow::anyhow!("could not connect to the X server: {e}"))?;
    let handle = backend.handle();

    // Render on the same GPU the X server hands us.
    let (drm_node, fd) = handle
        .drm_node()
        .map_err(|e| anyhow::anyhow!("X server exposes no usable DRM node: {e}"))?;
    tracing::info!(?drm_node, "nested X11 backend");

    let gbm = GbmDevice::new(DeviceFd::from(fd))?;
    let egl = unsafe { EGLDisplay::new(gbm.clone()) }?;
    let context = EGLContext::new(&egl)?;
    let renderer = unsafe { GlesRenderer::new(context) }?;

    let modifiers: Vec<_> = renderer
        .egl_context()
        .dmabuf_render_formats()
        .iter()
        .map(|format| format.modifier)
        .collect();

    let dh = display.handle();
    let mut outputs = HashMap::new();

    for index in 0..count {
        let window = WindowBuilder::new()
            .title(&format!("LineXinBar (virtual output {})", index + 1))
            .size((size.0 as u16, size.1 as u16).into())
            .build(&handle)
            .map_err(|e| anyhow::anyhow!("failed to create window {index}: {e}"))?;

        let surface = handle
            .create_surface(
                &window,
                // The X11 backend presents dmabufs, so wrap the gbm allocator.
                DmabufAllocator(GbmAllocator::new(gbm.clone(), GbmBufferFlags::RENDERING)),
                modifiers.iter().copied(),
            )
            .map_err(|e| anyhow::anyhow!("failed to create surface for window {index}: {e}"))?;

        let mode = Mode {
            size: (size.0, size.1).into(),
            refresh: 60_000,
        };
        let output = Output::new(
            format!("X11-{}", index + 1),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "LineXinBar".into(),
                model: "Virtual".into(),
            },
        );
        let global = output.create_global::<LxbState>(&dh);
        output.change_current_state(Some(mode), Some(Transform::Normal), None, None);
        output.set_preferred(mode);

        outputs.insert(
            window.id(),
            VirtualOutput {
                window,
                surface,
                damage_tracker: OutputDamageTracker::from_output(&output),
                output,
                _global: global,
                full_redraw: true,
            },
        );
    }

    let dmabuf_formats: Vec<_> = renderer.dmabuf_formats().iter().copied().collect();

    let mut state = LxbState::new(
        display,
        event_loop.handle(),
        event_loop.get_signal(),
        super::Backend::X11(Box::new(X11Backend {
            renderer,
            outputs,
            focused_windows: HashSet::new(),
            cursor: CursorState::new(),
            dmabuf_global: None,
        })),
        config,
        socket_name,
    )?;

    // Register every virtual output, letting the layout policy place them.
    let ordered: Vec<Output> = {
        let super::Backend::X11(x11) = &state.backend else {
            unreachable!()
        };
        let mut list: Vec<_> = x11.outputs.values().map(|o| o.output.clone()).collect();
        // HashMap iteration order is arbitrary; sort so `X11-1` is always leftmost.
        list.sort_by_key(|o| o.name());
        list
    };
    let config = state.lxb.config.clone();
    for output in &ordered {
        crate::outputs::OutputManager::apply_output_config(output, &config);
        state
            .lxb
            .outputs
            .add_output(&mut state.lxb.space, output, &config);
    }

    // With default feedback, which is what carries the global past version 3 —
    // measured at 5 here. The version is not a detail: feedback is where
    // `main_device` lives, and a client that cannot read that has no way to
    // know which GPU to allocate on. Firefox answers that by turning dmabuf off
    // entirely (`FEATURE_FAILURE_NO_DRM_DEVICE`) and falling back to software
    // rendering — silently, and only in the nested session, which makes every
    // GPU path here untestable and reads as a bug in whatever was being looked
    // at. The udev backend has always built the global this way; this is the
    // same call.
    if !dmabuf_formats.is_empty() {
        match DmabufFeedbackBuilder::new(drm_node.dev_id(), dmabuf_formats).build() {
            Ok(feedback) => {
                let global = state
                    .lxb
                    .dmabuf_state
                    .create_global_with_default_feedback::<LxbState>(
                        &state.lxb.display_handle,
                        &feedback,
                    );
                if let super::Backend::X11(x11) = &mut state.backend {
                    x11.dmabuf_global = Some(global);
                }
            }
            Err(err) => tracing::warn!(?err, "could not build dmabuf feedback"),
        }
    }

    state.lxb.pointer_location = (size.0 as f64 / 2.0, size.1 as f64 / 2.0).into();

    event_loop
        .handle()
        .insert_source(backend, |event, _, state| handle_event(state, event))
        .map_err(|e| anyhow::anyhow!("failed to insert X11 source: {e}"))?;

    event_loop
        .handle()
        .insert_source(Timer::immediate(), |_, _, state| {
            render_all(state);
            TimeoutAction::ToDuration(Duration::from_millis(16))
        })
        .map_err(|e| anyhow::anyhow!("failed to insert render timer: {e}"))?;

    tracing::info!(outputs = count, "nested X11 outputs ready");
    Ok(state)
}

fn handle_event(state: &mut LxbState, event: X11Event) {
    match event {
        X11Event::Input { event, window_id } => {
            let output = window_id.and_then(|window_id| {
                let super::Backend::X11(x11) = &state.backend else {
                    return None;
                };
                x11.outputs.get(&window_id).map(|o| o.output.clone())
            });
            state.process_input_event_for_output(event, output);
        }
        X11Event::Resized {
            new_size,
            window_id,
        } => {
            let mode = Mode {
                size: (new_size.w as i32, new_size.h as i32).into(),
                refresh: 60_000,
            };
            {
                let super::Backend::X11(x11) = &mut state.backend else {
                    return;
                };
                let Some(virtual_output) = x11.outputs.get_mut(&window_id) else {
                    return;
                };
                virtual_output
                    .output
                    .change_current_state(Some(mode), None, None, None);
                virtual_output.output.set_preferred(mode);
                virtual_output.full_redraw = true;
            }
            // A virtual display changed size, so the whole layout shifts.
            let config = state.lxb.config.clone();
            state.lxb.outputs.relayout(&mut state.lxb.space, &config);
        }
        X11Event::Refresh { window_id } | X11Event::PresentCompleted { window_id } => {
            if let super::Backend::X11(x11) = &mut state.backend {
                if let Some(virtual_output) = x11.outputs.get_mut(&window_id) {
                    virtual_output.full_redraw = true;
                }
            }
        }
        X11Event::CloseRequested { window_id } => {
            // Closing any one virtual display shuts the whole compositor down,
            // which matches what closing the single winit window does.
            tracing::info!(window_id, "virtual output closed");
            state.lxb.running = false;
            state.lxb.loop_signal.stop();
        }
        X11Event::Focus { focused, window_id } => {
            let (any_focused, output) = {
                let super::Backend::X11(x11) = &mut state.backend else {
                    return;
                };
                if focused {
                    x11.focused_windows.insert(window_id);
                } else {
                    x11.focused_windows.remove(&window_id);
                }
                (
                    !x11.focused_windows.is_empty(),
                    x11.outputs.get(&window_id).map(|o| o.output.clone()),
                )
            };

            if focused {
                state.nested_keyboard_focus_changed(true, output.as_ref());
            } else if !any_focused {
                state.nested_keyboard_focus_changed(false, None);
            }
        }
    }
}

fn render_all(state: &mut LxbState) {
    let window_ids: Vec<u32> = {
        let super::Backend::X11(x11) = &state.backend else {
            return;
        };
        x11.outputs.keys().copied().collect()
    };

    for window_id in window_ids {
        if let Err(err) = render_output(state, window_id) {
            tracing::warn!(window_id, ?err, "render failed");
        }
    }
}

fn render_output(state: &mut LxbState, window_id: u32) -> anyhow::Result<()> {
    let super::Backend::X11(x11) = &mut state.backend else {
        return Ok(());
    };
    // Destructuring gives disjoint borrows of the renderer and the output map,
    // so the output stays live across `bind` and needs looking up only once.
    let X11Backend {
        renderer,
        outputs,
        cursor,
        ..
    } = &mut **x11;

    cursor.status = state.lxb.cursor_status.clone();
    let draw_cursor = state.lxb.config.general.draw_cursor;
    let clear_color = state.lxb.config.general.background;

    let Some(virtual_output) = outputs.get_mut(&window_id) else {
        return Ok(());
    };
    let output = virtual_output.output.clone();

    let (mut buffer, age) = virtual_output
        .surface
        .buffer()
        .map_err(|e| anyhow::anyhow!("could not get a buffer: {e}"))?;
    let age = if virtual_output.full_redraw {
        0
    } else {
        age as usize
    };

    // Which is also the moment this frame's curtain was decided: read before
    // the elements are built, so a frame counted as black had the black over
    // every element in it. See [`crate::curtain::Curtain::a_frame_was_drawn`].
    let frame_started = std::time::Instant::now();
    let elements = output_elements(renderer, &state.lxb, &output, draw_cursor.then_some(cursor));

    let mut framebuffer = renderer
        .bind(&mut buffer)
        .map_err(|e| anyhow::anyhow!("bind failed: {e}"))?;

    let result = virtual_output
        .damage_tracker
        .render_output(renderer, &mut framebuffer, age, &elements, clear_color)
        .map_err(|e| anyhow::anyhow!("render_output failed: {e:?}"))?;

    drop(framebuffer);

    virtual_output
        .surface
        .submit()
        .map_err(|e| anyhow::anyhow!("submit failed: {e}"))?;
    virtual_output.full_redraw = false;

    let time = state.lxb.start_time.elapsed();
    state.lxb.curtain.a_frame_was_drawn(&output, frame_started);
    post_repaint(
        &state.lxb,
        &output,
        time,
        Some(Duration::ZERO),
        &result.states,
    );
    // The host X server never tells us when this frame is seen, so the
    // hand-over is the best answer there is — and far better than none.
    crate::render::answer_presentation_now(&state.lxb, &output, &result.states, time);
    // Anything else recording this display is answered here, with the screen
    // in the state it was just drawn in and a renderer already in hand.
    if state.lxb.screencopy.wanted(&output) {
        let LxbState { backend, lxb } = state;
        let super::Backend::X11(backend) = backend else {
            return Ok(());
        };
        let X11Backend {
            renderer, cursor, ..
        } = &mut **backend;
        let cursor = lxb.config.general.draw_cursor.then_some(cursor);
        crate::screencopy::serve(renderer, lxb, &output, cursor, time);
    }

    Ok(())
}
