//! Nested backend: one window inside an already running compositor.
//!
//! This is the debugging path. Linboard shows up as an ordinary Wayland client
//! of your desktop session, so it can be started, inspected and killed without
//! touching a TTY. Exactly one virtual output exists, matching the window.
//!
//! For nested *multi*-display debugging see [`super::x11`].

use std::time::Duration;

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
use smithay::wayland::dmabuf::DmabufGlobal;

use super::ImportError;
use crate::config::Config;
use crate::render::{output_elements, post_repaint, CursorState};
use crate::state::LinboardState;

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
}

/// Bring up the compositor on a nested winit window.
pub fn init(
    event_loop: &mut EventLoop<'static, LinboardState>,
    display: Display<LinboardState>,
    config: Config,
    socket_name: Option<String>,
) -> anyhow::Result<LinboardState> {
    let (graphics, winit_events) = winit::init_from_attributes::<GlesRenderer>(
        WinitWindow::default_attributes()
            .with_title("Linboard")
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
            make: "Linboard".into(),
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
    let output_global = output.create_global::<LinboardState>(&display.handle());

    let backend = super::Backend::Winit(Box::new(WinitBackend {
        backend: graphics,
        output: output.clone(),
        damage_tracker,
        cursor: CursorState::new(),
        dmabuf_global: None,
        _output_global: output_global,
    }));

    let mut state = LinboardState::new(
        display,
        event_loop.handle(),
        event_loop.get_signal(),
        backend,
        config,
        socket_name,
    )?;

    crate::outputs::OutputManager::apply_output_config(&output, &state.linboard.config);
    let config = state.linboard.config.clone();
    state
        .linboard
        .outputs
        .add_output(&mut state.linboard.space, &output, &config);

    // Advertise dmabuf so clients can hand us GPU buffers directly.
    if let super::Backend::Winit(winit_backend) = &mut state.backend {
        let formats: Vec<_> = winit_backend
            .backend
            .renderer()
            .dmabuf_formats()
            .iter()
            .copied()
            .collect();
        let global = state
            .linboard
            .dmabuf_state
            .create_global::<LinboardState>(&state.linboard.display_handle, formats);
        winit_backend.dmabuf_global = Some(global);
    }

    state.linboard.pointer_location = (size.w as f64 / 2.0, size.h as f64 / 2.0).into();

    insert_winit_source(event_loop.handle(), winit_events)?;
    insert_render_timer(event_loop.handle())?;

    Ok(state)
}

fn insert_winit_source(
    handle: LoopHandle<'static, LinboardState>,
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
                    let config = state.linboard.config.clone();
                    state
                        .linboard
                        .outputs
                        .relayout(&mut state.linboard.space, &config);
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
                    state.linboard.running = false;
                    state.linboard.loop_signal.stop();
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
fn insert_render_timer(handle: LoopHandle<'static, LinboardState>) -> anyhow::Result<()> {
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

fn render(state: &mut LinboardState) -> anyhow::Result<()> {
    let super::Backend::Winit(backend) = &mut state.backend else {
        return Ok(());
    };

    let output = backend.output.clone();
    let draw_cursor = state.linboard.config.general.draw_cursor;
    backend.cursor.status = state.linboard.cursor_status.clone();

    let elements = {
        let WinitBackend {
            backend: winit,
            cursor,
            ..
        } = &mut **backend;
        output_elements(
            winit.renderer(),
            &state.linboard,
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

    let clear_color = state.linboard.config.general.background;
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

    let time = state.linboard.start_time.elapsed();
    post_repaint(&state.linboard, &output, time, Some(Duration::ZERO));

    Ok(())
}
