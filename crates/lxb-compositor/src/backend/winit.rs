//! Nested backend: one window inside an already running compositor.
//!
//! This is the debugging path. LineXinBar shows up as an ordinary Wayland client
//! of your desktop session, so it can be started, inspected and killed without
//! touching a TTY. Exactly one virtual output exists, matching the window.
//!
//! For nested *multi*-display debugging see [`super::x11`].

use std::time::Duration;

use smithay::backend::egl::EGLDevice;
use smithay::backend::input::InputEvent;
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::ImportDma;
use smithay::backend::winit::{self, WinitEvent, WinitGraphicsBackend};
use smithay::output::{Mode, Output, PhysicalProperties, Subpixel};
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::calloop::{EventLoop, LoopHandle};
use smithay::reexports::wayland_server::backend::GlobalId;
use smithay::reexports::wayland_server::Display;
use smithay::reexports::winit::window::Window as WinitWindow;
use smithay::utils::{Rectangle, Transform};
use smithay::wayland::dmabuf::{DmabufFeedbackBuilder, DmabufGlobal};

use super::ImportError;
use crate::config::Config;
use crate::render::{output_elements, post_repaint, CursorState};
use crate::state::LxbState;

/// The nested winit backend.
pub struct WinitBackend {
    pub backend: WinitGraphicsBackend<GlesRenderer>,
    pub output: Output,
    pub damage_tracker: OutputDamageTracker,
    pub cursor: CursorState,
    pub dmabuf_global: Option<DmabufGlobal>,
    /// Held for the lifetime of the output; dropping it unadvertises wl_output.
    _output_global: GlobalId,
}

impl WinitBackend {
    pub fn import_dmabuf(
        &mut self,
        dmabuf: &smithay::backend::allocator::dmabuf::Dmabuf,
    ) -> Result<(), ImportError> {
        self.backend
            .renderer()
            .import_dmabuf(dmabuf, None)
            .map(|_| ())
            .map_err(|e| ImportError::Failed(e.to_string()))
    }

    pub fn capture_window(
        &mut self,
        window: &smithay::desktop::Window,
        scale: f64,
    ) -> anyhow::Result<crate::capture::Shot> {
        crate::capture::window(self.backend.renderer(), window, scale)
    }

    pub fn capture_output(
        &mut self,
        lxb: &crate::state::Lxb,
        output: &smithay::output::Output,
    ) -> anyhow::Result<crate::capture::Shot> {
        crate::capture::output(self.backend.renderer(), lxb, output)
    }

    /// Draw, small, what is on one side of the shell's own surfaces. See
    /// [`crate::capture::behind`].
    pub fn picture_behind(
        &mut self,
        lxb: &crate::state::Lxb,
        output: &smithay::output::Output,
        side: crate::capture::Side,
        size: smithay::utils::Size<i32, smithay::utils::Physical>,
    ) -> anyhow::Result<crate::capture::Shot> {
        crate::capture::behind(self.backend.renderer(), lxb, output, side, size)
    }
}

/// Bring up the compositor on a nested winit window.
pub fn init(
    event_loop: &mut EventLoop<'static, LxbState>,
    display: Display<LxbState>,
    config: Config,
    socket_name: Option<String>,
) -> anyhow::Result<LxbState> {
    let (graphics, winit_events) = winit::init_from_attributes::<GlesRenderer>(
        WinitWindow::default_attributes()
            .with_title("LineXinBar")
            .with_visible(true),
    )
    .map_err(|e| anyhow::anyhow!("failed to initialise winit backend: {e}"))?;

    let size = graphics.window_size();
    let mode = Mode {
        size,
        // 60Hz in mHz. The parent compositor actually paces us; this is only
        // what we advertise to clients.
        refresh: 60_000,
    };

    let output = Output::new(
        "WINIT-1".to_string(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "LineXinBar".into(),
            model: "Nested".into(),
        },
    );
    output.change_current_state(
        Some(mode),
        Some(Transform::Flipped180),
        None,
        Some((0, 0).into()),
    );
    output.set_preferred(mode);

    let damage_tracker = OutputDamageTracker::from_output(&output);

    // The output global has to outlive the backend, so create it up front from
    // the display handle rather than after the state exists.
    let output_global = output.create_global::<LxbState>(&display.handle());

    let backend = super::Backend::Winit(Box::new(WinitBackend {
        backend: graphics,
        output: output.clone(),
        damage_tracker,
        cursor: CursorState::new(),
        dmabuf_global: None,
        _output_global: output_global,
    }));

    let mut state = LxbState::new(
        display,
        event_loop.handle(),
        event_loop.get_signal(),
        backend,
        config,
        socket_name,
    )?;

    crate::outputs::OutputManager::apply_output_config(&output, &state.lxb.config);
    let config = state.lxb.config.clone();
    state
        .lxb
        .outputs
        .add_output(&mut state.lxb.space, &output, &config);

    // Advertise dmabuf so clients can hand us GPU buffers directly, with
    // default feedback for the reason [`super::x11`] gives: without it the
    // global is version 3, `main_device` is never sent, and a client that wants
    // to know which GPU to allocate on gives up and renders in software.
    if let super::Backend::Winit(winit_backend) = &mut state.backend {
        let renderer = winit_backend.backend.renderer();
        let formats: Vec<_> = renderer.dmabuf_formats().iter().copied().collect();
        let node = EGLDevice::device_for_display(renderer.egl_context().display())
            .and_then(|device| device.try_get_render_node());
        match node {
            Ok(Some(node)) => match DmabufFeedbackBuilder::new(node.dev_id(), formats).build() {
                Ok(feedback) => {
                    winit_backend.dmabuf_global = Some(
                        state
                            .lxb
                            .dmabuf_state
                            .create_global_with_default_feedback::<LxbState>(
                                &state.lxb.display_handle,
                                &feedback,
                            ),
                    );
                }
                Err(err) => tracing::warn!(?err, "could not build dmabuf feedback"),
            },
            // No render node to name means no feedback to build. The version 3
            // global still lets a client hand over buffers it has allocated
            // some other way, so it is worth advertising rather than dropping.
            other => {
                if let Err(err) = other {
                    tracing::warn!(?err, "could not find the render node for dmabuf feedback");
                }
                let global = state
                    .lxb
                    .dmabuf_state
                    .create_global::<LxbState>(&state.lxb.display_handle, formats);
                winit_backend.dmabuf_global = Some(global);
            }
        }
    }

    state.lxb.pointer_location = (size.w as f64 / 2.0, size.h as f64 / 2.0).into();

    insert_winit_source(event_loop.handle(), winit_events)?;
    insert_render_timer(event_loop.handle())?;

    Ok(state)
}

fn insert_winit_source(
    handle: LoopHandle<'static, LxbState>,
    winit_events: winit::WinitEventLoop,
) -> anyhow::Result<()> {
    handle
        .insert_source(winit_events, move |event, _, state| {
            let super::Backend::Winit(backend) = &mut state.backend else {
                return;
            };
            match event {
                WinitEvent::Resized { size, .. } => {
                    let mode = Mode {
                        size,
                        refresh: 60_000,
                    };
                    backend
                        .output
                        .change_current_state(Some(mode), None, None, None);
                    backend.output.set_preferred(mode);

                    // The virtual display changed size: redo the layout.
                    let config = state.lxb.config.clone();
                    state.lxb.outputs.relayout(&mut state.lxb.space, &config);
                }
                WinitEvent::Input(event) => {
                    let finish_touch_frame = matches!(
                        &event,
                        InputEvent::TouchDown { .. }
                            | InputEvent::TouchMotion { .. }
                            | InputEvent::TouchUp { .. }
                    );
                    let output = backend.output.clone();
                    let source_size = backend.backend.window_size();
                    state.process_input_event_from_window(event, output, source_size);
                    // Winit does not expose touch-frame events. Treat each host
                    // event as one complete atomic batch for Wayland clients.
                    if finish_touch_frame {
                        state.touch_frame();
                    }
                }
                WinitEvent::Focus(focused) => {
                    let output = backend.output.clone();
                    state.nested_keyboard_focus_changed(focused, Some(&output));
                }
                WinitEvent::CloseRequested => {
                    state.lxb.running = false;
                    state.lxb.loop_signal.stop();
                }
                WinitEvent::Redraw => {}
            }
        })
        .map_err(|e| anyhow::anyhow!("failed to insert winit source: {e}"))?;
    Ok(())
}

/// Drive repaints from a fixed timer.
///
/// A nested window has no page-flip event to schedule against, so a plain
/// ~60Hz timer is both the simplest and the most predictable option.
fn insert_render_timer(handle: LoopHandle<'static, LxbState>) -> anyhow::Result<()> {
    handle
        .insert_source(Timer::immediate(), move |_, _, state| {
            if let Err(err) = render(state) {
                tracing::warn!(?err, "render failed");
            }
            TimeoutAction::ToDuration(Duration::from_millis(16))
        })
        .map_err(|e| anyhow::anyhow!("failed to insert render timer: {e}"))?;
    Ok(())
}

fn render(state: &mut LxbState) -> anyhow::Result<()> {
    let super::Backend::Winit(backend) = &mut state.backend else {
        return Ok(());
    };

    let output = backend.output.clone();
    let draw_cursor = state.lxb.config.general.draw_cursor;
    backend.cursor.status = state.lxb.cursor_now();

    // Which is also the moment this frame's curtain was decided: read before
    // the elements are built, so a frame counted as black had the black over
    // every element in it. See [`crate::curtain::Curtain::a_frame_was_drawn`].
    let frame_started = std::time::Instant::now();
    let elements = {
        let WinitBackend {
            backend: winit,
            cursor,
            ..
        } = &mut **backend;
        output_elements(
            winit.renderer(),
            &state.lxb,
            &output,
            draw_cursor.then_some(cursor),
        )
    };

    // Always repaint in full here.
    //
    // `buffer_age` queries the EGL draw surface, which is only valid while that
    // surface is current, and `bind` hands back a framebuffer whose drop
    // unbinds it again — so any age we could ask for outside the bind would be
    // queried against an unbound surface and log `BAD_SURFACE` every frame.
    // This is the nested debugging backend drawing a single small window, so
    // skipping age-based damage tracking costs nothing worth reclaiming.
    let age = 0;

    let clear_color = state.lxb.config.general.background;
    let (renderer, mut framebuffer) = backend
        .backend
        .bind()
        .map_err(|e| anyhow::anyhow!("bind failed: {e}"))?;

    let result = backend
        .damage_tracker
        .render_output(renderer, &mut framebuffer, age, &elements, clear_color)
        .map_err(|e| anyhow::anyhow!("render_output failed: {e:?}"))?;

    drop(framebuffer);

    let damage: Option<Vec<Rectangle<i32, smithay::utils::Physical>>> =
        result.damage.map(|d| d.to_vec());
    backend
        .backend
        .submit(damage.as_deref())
        .map_err(|e| anyhow::anyhow!("submit failed: {e}"))?;

    let time = state.lxb.start_time.elapsed();
    state.lxb.curtain.a_frame_was_drawn(&output, frame_started);
    post_repaint(
        &state.lxb,
        &output,
        time,
        Some(Duration::ZERO),
        &result.states,
    );
    // The host compositor never tells us when this frame is seen, so the
    // hand-over is the best answer there is — and far better than none.
    crate::render::answer_presentation_now(&state.lxb, &output, &result.states, time);
    // Anything else recording this display is answered here, with the screen
    // in the state it was just drawn in and a renderer already in hand.
    if state.lxb.screencopy.wanted(&output) {
        let LxbState { backend, lxb } = state;
        let super::Backend::Winit(backend) = backend else {
            return Ok(());
        };
        let cursor = lxb
            .config
            .general
            .draw_cursor
            .then_some(&mut backend.cursor);
        crate::screencopy::serve(backend.backend.renderer(), lxb, &output, cursor, time);
    }

    Ok(())
}
