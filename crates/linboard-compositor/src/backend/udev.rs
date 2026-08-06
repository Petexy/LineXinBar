//! Native DRM/KMS backend.
//!
//! This is where the multi-display support actually lives. Every connected
//! connector becomes its own [`Output`] with its own CRTC, its own scanout
//! swapchain and its own vblank-driven render loop, so displays with different
//! resolutions and refresh rates run independently rather than being locked to
//! a common heartbeat.
//!
//! Multi-GPU is handled by smithay's [`GpuManager`]: rendering happens on the
//! primary GPU and the result is copied to whichever GPU actually drives the
//! connector, so a display plugged into a secondary card still works.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::allocator::gbm::{GbmAllocator, GbmBufferFlags, GbmDevice};
use smithay::backend::drm::compositor::{FrameError, FrameFlags};
use smithay::backend::drm::exporter::gbm::GbmFramebufferExporter;
use smithay::backend::drm::output::{DrmOutput, DrmOutputManager, DrmOutputRenderElements};
use smithay::backend::drm::{
    DrmDevice, DrmDeviceFd, DrmEvent, DrmEventMetadata, DrmEventTime, DrmNode, NodeType,
};
use smithay::backend::egl::EGLDevice;
use smithay::backend::input::InputEvent;
use smithay::backend::libinput::{LibinputInputBackend, LibinputSessionInterface};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::multigpu::gbm::GbmGlesBackend;
use smithay::backend::renderer::multigpu::{GpuManager, MultiRenderer};
use smithay::backend::renderer::ImportDma;
use smithay::backend::session::libseat::LibSeatSession;
use smithay::backend::session::{Event as SessionEvent, Session};
use smithay::backend::udev::{self, UdevEvent};
use smithay::desktop::utils::surface_primary_scanout_output;
use smithay::output::{Mode as OutputMode, Output, PhysicalProperties, Subpixel};
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::calloop::{EventLoop, RegistrationToken};
use smithay::reexports::drm::control::{connector, crtc, ModeTypeFlags};
use smithay::reexports::input::{Device as LibinputDevice, DeviceConfigError, Libinput};
use smithay::reexports::rustix::fs::OFlags;
use smithay::reexports::wayland_server::backend::GlobalId;
use smithay::reexports::wayland_server::Display;
use smithay::utils::DeviceFd;
use smithay::wayland::dmabuf::{DmabufFeedbackBuilder, DmabufGlobal};
use smithay::wayland::presentation::Refresh;
use smithay_drm_extras::drm_scanner::{DrmScanEvent, DrmScanner};

use super::ImportError;
use crate::config::{Config, Input as InputConfig, ModeRequest};
use crate::outputs::OutputManager;
use crate::render::{output_elements, post_repaint, CursorState, LinboardRenderElement};
use crate::state::LinboardState;

// Concrete instantiations of smithay's very generic DRM helpers.
type LinboardAllocator = GbmAllocator<DrmDeviceFd>;
type LinboardExporter = GbmFramebufferExporter<DrmDeviceFd>;
/// Attached to each queued frame and handed back on vblank.
type FrameUserData = Option<smithay::desktop::utils::OutputPresentationFeedback>;
type LinboardDrmOutput = DrmOutput<LinboardAllocator, LinboardExporter, FrameUserData, DrmDeviceFd>;
type LinboardDrmOutputManager =
    DrmOutputManager<LinboardAllocator, LinboardExporter, FrameUserData, DrmDeviceFd>;
type UdevGpus = GpuManager<GbmGlesBackend<GlesRenderer, DrmDeviceFd>>;
pub type UdevRenderer<'a> = MultiRenderer<
    'a,
    'a,
    GbmGlesBackend<GlesRenderer, DrmDeviceFd>,
    GbmGlesBackend<GlesRenderer, DrmDeviceFd>,
>;

/// Formats tried, in order, for a connector's primary plane.
const SUPPORTED_COLOR_FORMATS: &[smithay::backend::allocator::Fourcc] = &[
    smithay::backend::allocator::Fourcc::Argb8888,
    smithay::backend::allocator::Fourcc::Abgr8888,
    smithay::backend::allocator::Fourcc::Xrgb8888,
    smithay::backend::allocator::Fourcc::Xbgr8888,
];

/// Where a given output is in its render cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RenderState {
    /// Nothing scheduled.
    Idle,
    /// A render is queued on a timer.
    Scheduled,
    /// A frame is on its way to the screen; `dirty` records whether something
    /// changed while we were waiting, so we know to draw again on vblank.
    WaitingForVblank { dirty: bool },
}

/// One connector being driven.
struct SurfaceData {
    output: Output,
    global: Option<GlobalId>,
    drm_output: LinboardDrmOutput,
    render_state: RenderState,
}

/// One DRM device (one GPU).
struct DeviceData {
    surfaces: HashMap<crtc::Handle, SurfaceData>,
    drm_output_manager: LinboardDrmOutputManager,
    scanner: DrmScanner,
    render_node: DrmNode,
    registration_token: RegistrationToken,
}

pub struct UdevBackend {
    pub session: LibSeatSession,
    seat_name: String,
    primary_gpu: DrmNode,
    gpus: UdevGpus,
    devices: HashMap<DrmNode, DeviceData>,
    cursor: CursorState,
    dmabuf_global: Option<DmabufGlobal>,
}

impl UdevBackend {
    pub fn seat_name(&self) -> String {
        self.seat_name.clone()
    }

    pub fn switch_vt(&mut self, vt: i32) {
        if let Err(err) = self.session.change_vt(vt) {
            tracing::warn!(vt, ?err, "failed to switch VT");
        }
    }

    pub fn import_dmabuf(&mut self, dmabuf: &Dmabuf) -> Result<(), ImportError> {
        self.gpus
            .single_renderer(&self.primary_gpu)
            .map_err(|e| ImportError::Failed(e.to_string()))?
            .import_dmabuf(dmabuf, None)
            .map(|_| ())
            .map_err(|e| ImportError::Failed(e.to_string()))
    }
}

/// Bring up the compositor on real hardware.
pub fn init(
    event_loop: &mut EventLoop<'static, LinboardState>,
    display: Display<LinboardState>,
    config: Config,
    socket_name: Option<String>,
) -> anyhow::Result<LinboardState> {
    let (session, session_notifier) = LibSeatSession::new()
        .map_err(|e| anyhow::anyhow!("could not open a libseat session: {e}"))?;
    let seat_name = session.seat();
    tracing::info!(seat = %seat_name, "opened session");

    // Prefer the GPU logind hands us; fall back to any GPU that can render.
    let primary_gpu = match udev::primary_gpu(&seat_name)? {
        Some(path) => renderer_node(DrmNode::from_path(&path)?),
        None => {
            let path = udev::all_gpus(&seat_name)?
                .into_iter()
                .next()
                .ok_or_else(|| anyhow::anyhow!("no GPU found for seat {seat_name}"))?;
            renderer_node(DrmNode::from_path(&path)?)
        }
    };
    tracing::info!(gpu = ?primary_gpu, "primary GPU");

    let gpus = GpuManager::new(GbmGlesBackend::default())
        .map_err(|e| anyhow::anyhow!("failed to set up multi-GPU renderer: {e}"))?;

    let backend = super::Backend::Udev(Box::new(UdevBackend {
        session: session.clone(),
        seat_name: seat_name.clone(),
        primary_gpu,
        gpus,
        devices: HashMap::new(),
        cursor: CursorState::new(),
        dmabuf_global: None,
    }));

    let mut state = LinboardState::new(
        display,
        event_loop.handle(),
        event_loop.get_signal(),
        backend,
        config,
        socket_name,
    )?;

    // libinput, sharing the session so device fds survive VT switches.
    let mut libinput =
        Libinput::new_with_udev::<LibinputSessionInterface<LibSeatSession>>(session.clone().into());
    libinput
        .udev_assign_seat(&seat_name)
        .map_err(|_| anyhow::anyhow!("failed to assign libinput seat {seat_name}"))?;

    event_loop
        .handle()
        .insert_source(
            LibinputInputBackend::new(libinput.clone()),
            |event, _, state| {
                if let InputEvent::DeviceAdded { device } = &event {
                    configure_libinput_device(device, &state.linboard.config.input);
                }
                state.process_input_event(event);
            },
        )
        .map_err(|e| anyhow::anyhow!("failed to insert libinput source: {e}"))?;

    // VT switches: release/reacquire every device.
    event_loop
        .handle()
        .insert_source(session_notifier, move |event, _, state| match event {
            SessionEvent::PauseSession => {
                tracing::info!("session paused");
                libinput.suspend();
                if let super::Backend::Udev(udev) = &mut state.backend {
                    for device in udev.devices.values_mut() {
                        device.drm_output_manager.pause();
                    }
                }
            }
            SessionEvent::ActivateSession => {
                tracing::info!("session resumed");
                if libinput.resume().is_err() {
                    tracing::error!("failed to resume libinput");
                }
                let crtcs = {
                    let super::Backend::Udev(udev) = &mut state.backend else {
                        return;
                    };
                    let mut crtcs = Vec::new();
                    for (node, device) in udev.devices.iter_mut() {
                        if let Err(err) = device.drm_output_manager.activate(false) {
                            tracing::warn!(?err, "failed to reactivate DRM device");
                        }
                        for (crtc, surface) in device.surfaces.iter_mut() {
                            surface.render_state = RenderState::Idle;
                            crtcs.push((*node, *crtc));
                        }
                    }
                    crtcs
                };
                // Force a fresh frame everywhere now that we own the GPU again.
                for (node, crtc) in crtcs {
                    schedule_render(state, node, crtc, Duration::ZERO);
                }
            }
        })
        .map_err(|e| anyhow::anyhow!("failed to insert session source: {e}"))?;

    // Enumerate GPUs that are already present, then watch for hotplug.
    let udev_backend = udev::UdevBackend::new(&seat_name)?;
    for (device_id, path) in udev_backend.device_list() {
        if let Ok(node) = DrmNode::from_dev_id(device_id) {
            if let Err(err) = device_added(&mut state, node, path) {
                tracing::warn!(?node, ?err, "skipping GPU");
            }
        }
    }

    event_loop
        .handle()
        .insert_source(udev_backend, move |event, _, state| match event {
            UdevEvent::Added { device_id, path } => {
                if let Ok(node) = DrmNode::from_dev_id(device_id) {
                    if let Err(err) = device_added(state, node, &path) {
                        tracing::warn!(?node, ?err, "failed to add GPU");
                    }
                }
            }
            UdevEvent::Changed { device_id } => {
                if let Ok(node) = DrmNode::from_dev_id(device_id) {
                    device_changed(state, node);
                }
            }
            UdevEvent::Removed { device_id } => {
                if let Ok(node) = DrmNode::from_dev_id(device_id) {
                    device_removed(state, node);
                }
            }
        })
        .map_err(|e| anyhow::anyhow!("failed to insert udev source: {e}"))?;

    // dmabuf, advertising the primary GPU's formats with a default feedback.
    let (primary, formats) = {
        let super::Backend::Udev(udev) = &mut state.backend else {
            unreachable!("the udev backend was just constructed")
        };
        let primary = udev.primary_gpu;
        let formats = udev
            .gpus
            .single_renderer(&primary)
            .map(|r| r.dmabuf_formats())
            .unwrap_or_default();
        (primary, formats)
    };
    if formats.iter().next().is_some() {
        let feedback = DmabufFeedbackBuilder::new(primary.dev_id(), formats).build();
        match feedback {
            Ok(feedback) => {
                let global = state
                    .linboard
                    .dmabuf_state
                    .create_global_with_default_feedback::<LinboardState>(
                        &state.linboard.display_handle,
                        &feedback,
                    );
                if let super::Backend::Udev(udev) = &mut state.backend {
                    udev.dmabuf_global = Some(global);
                }
            }
            Err(err) => tracing::warn!(?err, "could not build dmabuf feedback"),
        }
    }

    if state.linboard.outputs.is_empty() {
        anyhow::bail!("no usable display found; is a monitor connected?");
    }

    Ok(state)
}

/// Apply the user-facing input preferences to a newly discovered libinput
/// device. Unsupported settings are normal (for example tapping on a mouse),
/// so availability is checked before each call and only genuine failures are
/// logged.
fn configure_libinput_device(device: &LibinputDevice, config: &InputConfig) {
    let mut device = device.clone();
    let name = device.name().to_string();

    let report = |setting: &'static str, result: Result<(), DeviceConfigError>| {
        if let Err(err) = result {
            tracing::warn!(device = %name, setting, ?err, "could not apply input setting");
        }
    };

    if device.config_tap_finger_count() > 0 {
        report(
            "tap-to-click",
            device.config_tap_set_enabled(config.tap_to_click),
        );
    }
    if device.config_scroll_has_natural_scroll() {
        report(
            "natural-scroll",
            device.config_scroll_set_natural_scroll_enabled(config.natural_scroll),
        );
    }
    if device.config_dwt_is_available() {
        report(
            "disable-while-typing",
            device.config_dwt_set_enabled(config.disable_while_typing),
        );
    }
    if device.config_accel_is_available() {
        let speed = config.pointer_accel.clamp(-1.0, 1.0);
        if speed != config.pointer_accel {
            tracing::warn!(
                device = %name,
                requested = config.pointer_accel,
                applied = speed,
                "pointer acceleration is outside libinput's supported range"
            );
        }
        report("pointer-accel", device.config_accel_set_speed(speed));
    }
}

/// Return the render node belonging to a DRM device when it has one.
///
/// Udev enumerates primary (`cardN`) nodes, while `GpuManager` is keyed by the
/// EGL render node discovered in [`device_added`].  Normalising the selected
/// primary GPU here keeps renderer lookup, dmabuf import and feedback on the
/// same key.  Drivers without a separate render node continue to use their
/// primary node.
fn renderer_node(node: DrmNode) -> DrmNode {
    node.node_with_type(NodeType::Render)
        .and_then(Result::ok)
        .unwrap_or(node)
}

// ---------------------------------------------------------------------------
// device lifecycle
// ---------------------------------------------------------------------------

fn device_added(state: &mut LinboardState, node: DrmNode, path: &Path) -> anyhow::Result<()> {
    let super::Backend::Udev(udev) = &mut state.backend else {
        return Ok(());
    };
    if udev.devices.contains_key(&node) {
        return Ok(());
    }

    let fd = udev.session.open(
        path,
        OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK,
    )?;
    let fd = DrmDeviceFd::new(DeviceFd::from(fd));

    let (drm, drm_notifier) = DrmDevice::new(fd.clone(), true)?;
    let gbm = GbmDevice::new(fd)?;

    // The render node may differ from the primary node (e.g. split render/display).
    let render_node = EGLDevice::device_for_display(&unsafe {
        smithay::backend::egl::EGLDisplay::new(gbm.clone())?
    })
    .ok()
    .and_then(|device| device.try_get_render_node().ok().flatten())
    .unwrap_or(node);

    udev.gpus
        .as_mut()
        .add_node(render_node, gbm.clone())
        .map_err(|e| anyhow::anyhow!("failed to register GPU with the renderer: {e}"))?;

    let registration_token = state
        .linboard
        .loop_handle
        .insert_source(drm_notifier, move |event, metadata, state| match event {
            DrmEvent::VBlank(crtc) => on_vblank(state, node, crtc, metadata),
            DrmEvent::Error(err) => tracing::warn!(?err, "DRM error"),
        })
        .map_err(|e| anyhow::anyhow!("failed to insert DRM source: {e}"))?;

    let allocator = GbmAllocator::new(
        gbm.clone(),
        GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
    );
    let renderer_formats = udev
        .gpus
        .single_renderer(&render_node)
        .map(|r| r.dmabuf_formats())
        .unwrap_or_default();

    let drm_output_manager = DrmOutputManager::new(
        drm,
        allocator,
        GbmFramebufferExporter::new(gbm.clone(), Some(render_node)),
        Some(gbm.clone()),
        SUPPORTED_COLOR_FORMATS.iter().copied(),
        renderer_formats,
    );

    udev.devices.insert(
        node,
        DeviceData {
            surfaces: HashMap::new(),
            drm_output_manager,
            scanner: DrmScanner::new(),
            render_node,
            registration_token,
        },
    );

    tracing::info!(?node, ?render_node, "GPU added");
    device_changed(state, node);
    Ok(())
}

/// Rescan a device's connectors. Called on hotplug and after adding a device.
fn device_changed(state: &mut LinboardState, node: DrmNode) {
    let scan = {
        let super::Backend::Udev(udev) = &mut state.backend else {
            return;
        };
        let Some(device) = udev.devices.get_mut(&node) else {
            return;
        };
        match device
            .scanner
            .scan_connectors(device.drm_output_manager.device())
        {
            Ok(scan) => scan,
            Err(err) => {
                tracing::warn!(?node, ?err, "failed to scan connectors");
                return;
            }
        }
    };

    for event in scan.iter() {
        match event {
            DrmScanEvent::Connected {
                connector,
                crtc: Some(crtc),
            } => connector_connected(state, node, connector, crtc),
            DrmScanEvent::Disconnected {
                connector,
                crtc: Some(crtc),
            } => connector_disconnected(state, node, connector, crtc),
            _ => {}
        }
    }
}

fn device_removed(state: &mut LinboardState, node: DrmNode) {
    let crtcs: Vec<_> = {
        let super::Backend::Udev(udev) = &state.backend else {
            return;
        };
        let Some(device) = udev.devices.get(&node) else {
            return;
        };
        device.surfaces.keys().copied().collect()
    };

    for crtc in crtcs {
        // The connector info is gone by now; unmap by CRTC directly.
        remove_surface(state, node, crtc);
    }

    let super::Backend::Udev(udev) = &mut state.backend else {
        return;
    };
    if let Some(device) = udev.devices.remove(&node) {
        udev.gpus.as_mut().remove_node(&device.render_node);
        state.linboard.loop_handle.remove(device.registration_token);
        tracing::info!(?node, "GPU removed");
    }
}

/// A display was plugged in (or was already there at startup).
fn connector_connected(
    state: &mut LinboardState,
    node: DrmNode,
    connector: connector::Info,
    crtc: crtc::Handle,
) {
    let name = format!(
        "{}-{}",
        connector.interface().as_str(),
        connector.interface_id()
    );

    if !OutputManager::is_enabled(&name, &state.linboard.config) {
        tracing::info!(output = %name, "output disabled by config, leaving it dark");
        return;
    }

    let Some(mode) = pick_mode(
        &connector,
        state
            .linboard
            .config
            .output_for(&name)
            .and_then(|c| c.parse_mode()),
    ) else {
        tracing::warn!(output = %name, "connector has no usable mode");
        return;
    };

    // smithay's conversion also handles interlaced, doublescan and vscan
    // modes, which a naive clock/htotal/vtotal calculation gets wrong.
    let output_mode = OutputMode::from(mode);
    let (w, h) = (output_mode.size.w, output_mode.size.h);
    let refresh = output_mode.refresh;

    let (physical_w, physical_h) = connector.size().unwrap_or((0, 0));
    let output = Output::new(
        name.clone(),
        PhysicalProperties {
            size: (physical_w as i32, physical_h as i32).into(),
            subpixel: Subpixel::Unknown,
            make: "Unknown".into(),
            model: connector.interface().as_str().to_string(),
        },
    );
    let global = output.create_global::<LinboardState>(&state.linboard.display_handle);
    output.set_preferred(output_mode);
    output.change_current_state(Some(output_mode), None, None, None);
    OutputManager::apply_output_config(&output, &state.linboard.config);

    let super::Backend::Udev(udev) = &mut state.backend else {
        return;
    };
    let Some(device) = udev.devices.get_mut(&node) else {
        return;
    };
    let render_node = device.render_node;

    let mut renderer = match udev.gpus.single_renderer(&render_node) {
        Ok(renderer) => renderer,
        Err(err) => {
            tracing::warn!(?err, output = %name, "no renderer for this GPU");
            return;
        }
    };

    let drm_output = match device
        .drm_output_manager
        .initialize_output::<_, LinboardRenderElement<UdevRenderer<'_>>>(
            crtc,
            mode,
            &[connector.handle()],
            &output,
            None,
            &mut renderer,
            &DrmOutputRenderElements::default(),
        ) {
        Ok(drm_output) => drm_output,
        Err(err) => {
            tracing::warn!(?err, output = %name, "failed to initialise output");
            return;
        }
    };
    drop(renderer);

    device.surfaces.insert(
        crtc,
        SurfaceData {
            output: output.clone(),
            global: Some(global),
            drm_output,
            render_state: RenderState::Idle,
        },
    );

    let config = state.linboard.config.clone();
    state
        .linboard
        .outputs
        .add_output(&mut state.linboard.space, &output, &config);

    tracing::info!(
        output = %name,
        width = w,
        height = h,
        refresh_mhz = refresh,
        "display connected"
    );

    schedule_render(state, node, crtc, Duration::ZERO);
}

fn connector_disconnected(
    state: &mut LinboardState,
    node: DrmNode,
    connector: connector::Info,
    crtc: crtc::Handle,
) {
    tracing::info!(
        output = format!(
            "{}-{}",
            connector.interface().as_str(),
            connector.interface_id()
        ),
        "display disconnected"
    );
    remove_surface(state, node, crtc);
}

fn remove_surface(state: &mut LinboardState, node: DrmNode, crtc: crtc::Handle) {
    let surface = {
        let super::Backend::Udev(udev) = &mut state.backend else {
            return;
        };
        let Some(device) = udev.devices.get_mut(&node) else {
            return;
        };
        device.surfaces.remove(&crtc)
    };

    let Some(surface) = surface else { return };

    let config = state.linboard.config.clone();
    state
        .linboard
        .outputs
        .remove_output(&mut state.linboard.space, &surface.output, &config);

    if let Some(global) = surface.global {
        state
            .linboard
            .display_handle
            .remove_global::<LinboardState>(global);
    }
}

/// Choose a mode: the configured one if it matches, else the connector's
/// preferred mode, else the first one it offers.
fn pick_mode(
    connector: &connector::Info,
    requested: Option<ModeRequest>,
) -> Option<smithay::reexports::drm::control::Mode> {
    let modes = connector.modes();

    if let Some(want) = requested {
        let mut matching: Vec<_> = modes
            .iter()
            .filter(|m| {
                let (w, h) = m.size();
                w as i32 == want.width && h as i32 == want.height
            })
            .collect();

        if let Some(refresh) = want.refresh {
            // Closest refresh rate wins, so `@60` matches a 59.94Hz mode.
            matching.sort_by_key(|m| (OutputMode::from(**m).refresh - refresh).abs());
        } else {
            matching.sort_by_key(|m| std::cmp::Reverse(OutputMode::from(**m).refresh));
        }

        if let Some(mode) = matching.first() {
            return Some(**mode);
        }
        tracing::warn!(
            ?want,
            "requested mode unavailable, falling back to preferred"
        );
    }

    modes
        .iter()
        .find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED))
        .or_else(|| modes.first())
        .copied()
}

// ---------------------------------------------------------------------------
// rendering
// ---------------------------------------------------------------------------

fn schedule_render(state: &mut LinboardState, node: DrmNode, crtc: crtc::Handle, delay: Duration) {
    {
        let super::Backend::Udev(udev) = &mut state.backend else {
            return;
        };
        let Some(surface) = udev
            .devices
            .get_mut(&node)
            .and_then(|d| d.surfaces.get_mut(&crtc))
        else {
            return;
        };

        match surface.render_state {
            // Already going to draw; nothing to do but remember it is dirty.
            RenderState::Scheduled => return,
            RenderState::WaitingForVblank { .. } => {
                surface.render_state = RenderState::WaitingForVblank { dirty: true };
                return;
            }
            RenderState::Idle => surface.render_state = RenderState::Scheduled,
        }
    }

    let timer = if delay.is_zero() {
        Timer::immediate()
    } else {
        Timer::from_duration(delay)
    };

    let res = state
        .linboard
        .loop_handle
        .insert_source(timer, move |_, _, state| {
            render_surface(state, node, crtc);
            TimeoutAction::Drop
        });
    if let Err(err) = res {
        tracing::warn!(?err, "failed to schedule a render");
    }
}

fn render_surface(state: &mut LinboardState, node: DrmNode, crtc: crtc::Handle) {
    let super::Backend::Udev(udev) = &mut state.backend else {
        return;
    };
    let Some(device) = udev.devices.get_mut(&node) else {
        return;
    };
    let render_node = device.render_node;
    let Some(surface) = device.surfaces.get_mut(&crtc) else {
        return;
    };

    surface.render_state = RenderState::Idle;

    let output = surface.output.clone();
    let primary_gpu = udev.primary_gpu;

    udev.cursor.status = state.linboard.cursor_status.clone();
    let draw_cursor = state.linboard.config.general.draw_cursor;
    let clear_color = state.linboard.config.general.background;

    // Render on the primary GPU, present on whichever GPU drives the connector.
    let renderer = if primary_gpu == render_node {
        udev.gpus.single_renderer(&render_node)
    } else {
        udev.gpus
            .renderer(&primary_gpu, &render_node, surface.drm_output.format())
    };
    let mut renderer = match renderer {
        Ok(renderer) => renderer,
        Err(err) => {
            tracing::warn!(?err, output = output.name(), "no renderer available");
            // The render state was reset to Idle above, so without rescheduling
            // this output would never draw again.
            schedule_render(state, node, crtc, Duration::from_millis(16));
            return;
        }
    };

    let elements = output_elements(
        &mut renderer,
        &state.linboard,
        &output,
        draw_cursor.then_some(&mut udev.cursor),
    );

    let refresh = output
        .current_mode()
        .map(|m| Duration::from_secs_f64(1000.0 / m.refresh as f64))
        .unwrap_or(Duration::from_millis(16));

    let result =
        surface
            .drm_output
            .render_frame(&mut renderer, &elements, clear_color, FrameFlags::DEFAULT);

    drop(renderer);

    match result {
        Ok(render_result) => {
            let feedback = if render_result.is_empty {
                None
            } else {
                Some(presentation_feedback(state, &output))
            };

            // Queue inside a scope so the surface borrow ends before the
            // reschedule below, which needs the whole state again.
            let queued = {
                let super::Backend::Udev(udev) = &mut state.backend else {
                    return;
                };
                let Some(surface) = udev
                    .devices
                    .get_mut(&node)
                    .and_then(|d| d.surfaces.get_mut(&crtc))
                else {
                    return;
                };

                match surface.drm_output.queue_frame(feedback) {
                    Ok(()) => {
                        surface.render_state = RenderState::WaitingForVblank { dirty: false };
                        true
                    }
                    // Nothing actually changed on screen.
                    Err(FrameError::EmptyFrame) => false,
                    Err(err) => {
                        tracing::warn!(?err, output = output.name(), "failed to queue frame");
                        false
                    }
                }
            };

            // Not submitted, so no vblank is coming: poll again next retrace.
            if !queued {
                schedule_render(state, node, crtc, refresh);
            }
        }
        Err(err) => {
            tracing::warn!(?err, output = output.name(), "failed to render frame");
            schedule_render(state, node, crtc, refresh);
        }
    }

    let time = state.linboard.start_time.elapsed();
    post_repaint(&state.linboard, &output, time, None);
}

/// Collect the presentation feedback for everything visible on `output`.
fn presentation_feedback(
    state: &LinboardState,
    output: &Output,
) -> smithay::desktop::utils::OutputPresentationFeedback {
    let mut feedback = smithay::desktop::utils::OutputPresentationFeedback::new(output);

    for window in state.linboard.space.elements_for_output(output) {
        window.take_presentation_feedback(
            &mut feedback,
            surface_primary_scanout_output,
            |surface, _| {
                smithay::desktop::utils::surface_presentation_feedback_flags_from_states(
                    surface,
                    &Default::default(),
                )
            },
        );
    }

    for layer in smithay::desktop::layer_map_for_output(output).layers() {
        layer.take_presentation_feedback(
            &mut feedback,
            surface_primary_scanout_output,
            |surface, _| {
                smithay::desktop::utils::surface_presentation_feedback_flags_from_states(
                    surface,
                    &Default::default(),
                )
            },
        );
    }

    feedback
}

fn on_vblank(
    state: &mut LinboardState,
    node: DrmNode,
    crtc: crtc::Handle,
    metadata: &mut Option<DrmEventMetadata>,
) {
    let (dirty, refresh) = {
        let super::Backend::Udev(udev) = &mut state.backend else {
            return;
        };
        let Some(surface) = udev
            .devices
            .get_mut(&node)
            .and_then(|d| d.surfaces.get_mut(&crtc))
        else {
            return;
        };

        let dirty = matches!(
            surface.render_state,
            RenderState::WaitingForVblank { dirty: true }
        );
        surface.render_state = RenderState::Idle;

        // Hand the frame back to the swapchain and answer presentation feedback.
        match surface.drm_output.frame_submitted() {
            Ok(Some(Some(mut feedback))) => {
                let seq = metadata.as_ref().map(|m| m.sequence as u64).unwrap_or(0);
                let time = match metadata.as_ref().map(|m| m.time) {
                    Some(DrmEventTime::Monotonic(t)) => t,
                    _ => state.linboard.start_time.elapsed(),
                };
                let refresh = surface
                    .output
                    .current_mode()
                    .map(|m| Refresh::fixed(Duration::from_secs_f64(1000.0 / m.refresh as f64)))
                    .unwrap_or(Refresh::Unknown);

                feedback.presented::<_, smithay::utils::Monotonic>(
                    time,
                    refresh,
                    seq,
                    smithay::reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback::Kind::Vsync,
                );
            }
            Ok(_) => {}
            Err(err) => tracing::warn!(?err, "frame_submitted failed"),
        }

        let refresh = surface
            .output
            .current_mode()
            .map(|m| Duration::from_secs_f64(1000.0 / m.refresh as f64))
            .unwrap_or(Duration::from_millis(16));
        (dirty, refresh)
    };

    // Redraw immediately if something changed mid-flight, otherwise poll again
    // one retrace later so newly damaged clients are picked up promptly.
    schedule_render(
        state,
        node,
        crtc,
        if dirty { Duration::ZERO } else { refresh },
    );
}

/// Ask every output to draw again. Called when compositor state changes.
pub fn queue_redraw_all(state: &mut LinboardState) {
    let targets: Vec<(DrmNode, crtc::Handle)> = {
        let super::Backend::Udev(udev) = &state.backend else {
            return;
        };
        udev.devices
            .iter()
            .flat_map(|(node, device)| device.surfaces.keys().map(move |crtc| (*node, *crtc)))
            .collect()
    };
    for (node, crtc) in targets {
        schedule_render(state, node, crtc, Duration::ZERO);
    }
}
