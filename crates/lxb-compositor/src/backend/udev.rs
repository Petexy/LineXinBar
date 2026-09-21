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
use std::os::fd::{AsFd, AsRawFd, RawFd};
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
use smithay::backend::input::{Event as InputEventTrait, InputEvent};
use smithay::backend::libinput::{LibinputInputBackend, LibinputSessionInterface};
use smithay::backend::renderer::element::RenderElementStates;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::multigpu::gbm::GbmGlesBackend;
use smithay::backend::renderer::multigpu::{GpuManager, MultiRenderer};
use smithay::backend::renderer::ImportDma;
use smithay::backend::session::libseat::LibSeatSession;
use smithay::backend::session::{Event as SessionEvent, Session};
use smithay::backend::udev::{self, UdevEvent};
use smithay::output::{Mode as OutputMode, Output, PhysicalProperties, Subpixel};
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::calloop::{EventLoop, RegistrationToken};
use smithay::reexports::drm::control::{connector, crtc, ModeTypeFlags};
use smithay::reexports::input::{
    Device as LibinputDevice, DeviceCapability, DeviceConfigError, Libinput,
};
use smithay::reexports::rustix::fs::OFlags;
use smithay::reexports::wayland_server::backend::GlobalId;
use smithay::reexports::wayland_server::Display;
use smithay::utils::DeviceFd;
use smithay::wayland::dmabuf::{DmabufFeedbackBuilder, DmabufGlobal};
use smithay::wayland::drm_syncobj::{supports_syncobj_eventfd, DrmSyncobjState};
use smithay::wayland::presentation::Refresh;
use smithay_drm_extras::drm_scanner::{DrmScanEvent, DrmScanner};

use super::ImportError;
use crate::config::{Config, Input as InputConfig, ModeRequest};
use crate::outputs::{DisplayMode, OutputManager};
use crate::render::{output_elements, post_repaint, CursorState, LxbRenderElement};
use crate::state::LxbState;

// Concrete instantiations of smithay's very generic DRM helpers.
type LxbAllocator = GbmAllocator<DrmDeviceFd>;
type LxbExporter = GbmFramebufferExporter<DrmDeviceFd>;
/// Attached to each queued frame and handed back on vblank.
type FrameUserData = Option<smithay::desktop::utils::OutputPresentationFeedback>;
type LxbDrmOutput = DrmOutput<LxbAllocator, LxbExporter, FrameUserData, DrmDeviceFd>;
type LxbDrmOutputManager = DrmOutputManager<LxbAllocator, LxbExporter, FrameUserData, DrmDeviceFd>;
type UdevGpus = GpuManager<GbmGlesBackend<GlesRenderer, DrmDeviceFd>>;
pub type UdevRenderer<'a> = MultiRenderer<
    'a,
    'a,
    GbmGlesBackend<GlesRenderer, DrmDeviceFd>,
    GbmGlesBackend<GlesRenderer, DrmDeviceFd>,
>;

/// Formats tried, in order, for a connector's primary plane.
///
/// Ten bits a channel first, and not only for HDR's sake — though HDR needs it
/// badly, because PQ spends most of its codes below SDR white and an eight-bit
/// framebuffer re-encoded through it bands visibly in every dark gradient. An
/// SDR session is better off on it too: the same gradients, drawn by the same
/// shell, simply have more levels to land on. Smithay walks this list until
/// something the driver will scan out comes back, so hardware without a 10-bit
/// primary plane falls through to the eight-bit formats by itself.
const SUPPORTED_COLOR_FORMATS: &[smithay::backend::allocator::Fourcc] = &[
    smithay::backend::allocator::Fourcc::Argb2101010,
    smithay::backend::allocator::Fourcc::Abgr2101010,
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
    drm_output: LxbDrmOutput,
    render_state: RenderState,
    /// When the frame now on its way to the screen was handed to the kernel.
    ///
    /// Only so that [`watch_for_a_lost_flip`] can tell one queued frame from
    /// the next: a page flip whose event never arrives leaves this surface
    /// waiting for a vblank that is not coming, and nothing else would ever say
    /// so — the display keeps showing the last frame, every client on it stops
    /// being sent frame callbacks, and the session is over without a single
    /// line in the log.
    queued_at: Option<std::time::Instant>,
    /// The connector itself, which HDR is signalled on — the CRTC only carries
    /// the colour pipeline half of it.
    connector: connector::Handle,
    /// Every mode this connector offers, as it listed them when the cable went
    /// in. Kept rather than re-probed: the list only changes when a display is
    /// plugged or unplugged, and both of those rebuild this whole surface.
    modes: Vec<smithay::reexports::drm::control::Mode>,
    /// The five KMS properties HDR is made of on this connector, and the
    /// display's own claims about what it can show.
    hdr: crate::hdr::Pipeline,
    hdr_display: crate::hdr::Display,
}

/// One DRM device (one GPU).
struct DeviceData {
    surfaces: HashMap<crtc::Handle, SurfaceData>,
    drm_output_manager: LxbDrmOutputManager,
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
    /// Every pointing device libinput has handed over, so the settings can be
    /// re-applied to the ones already plugged in and not only to the next one.
    ///
    /// libinput has no way to enumerate what it has opened — a device only ever
    /// arrives as an event — so the list has to be kept as those events go by.
    /// Held rather than looked up because Settings > Input > Mouse changes
    /// while somebody is watching the pointer they are changing: a speed that
    /// waited for a replug would read as a press that did nothing.
    pointing: Vec<LibinputDevice>,
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

    /// Hand the pointing settings to every device that is already open.
    ///
    /// The same call [`configure_libinput_device`] makes for a device arriving,
    /// over the list of the ones that arrived earlier — so what a mouse plugged
    /// in this morning does and what one plugged in now does are decided in one
    /// place and cannot drift apart.
    pub fn apply_input_settings(&mut self, config: &InputConfig) {
        for device in &self.pointing {
            configure_libinput_device(device, config);
        }
    }

    /// Draw the compositor's own cursor at this many logical pixels.
    pub fn set_cursor_size(&mut self, size: u32) {
        self.cursor.set_size(size);
    }

    pub fn import_dmabuf(&mut self, dmabuf: &Dmabuf) -> Result<(), ImportError> {
        self.gpus
            .single_renderer(&self.primary_gpu)
            .map_err(|e| ImportError::Failed(e.to_string()))?
            .import_dmabuf(dmabuf, None)
            .map(|_| ())
            .map_err(|e| ImportError::Failed(e.to_string()))
    }

    /// On the primary GPU, whichever one the window is being scanned out from.
    ///
    /// The picture is read back into main memory, so it does not matter which
    /// device drew it — and the primary is the one every client's buffers are
    /// already importable on, which is what a window on a secondary GPU needs.
    pub fn capture_window(
        &mut self,
        window: &smithay::desktop::Window,
        scale: f64,
    ) -> anyhow::Result<crate::capture::Shot> {
        let mut renderer = self
            .gpus
            .single_renderer(&self.primary_gpu)
            .map_err(|err| anyhow::anyhow!("no renderer to photograph the window with: {err}"))?;
        crate::capture::window(&mut renderer, window, scale)
    }

    /// On the primary GPU, whichever one this display hangs off.
    ///
    /// The same choice [`Self::capture_window`] makes, and for the same reason:
    /// the picture is read back into main memory, and the primary is where
    /// every client's buffers are importable. A display driven by a second GPU
    /// is composited here and copied, exactly as its own frames are.
    pub fn capture_output(
        &mut self,
        lxb: &crate::state::Lxb,
        output: &Output,
    ) -> anyhow::Result<crate::capture::Shot> {
        let mut renderer = self
            .gpus
            .single_renderer(&self.primary_gpu)
            .map_err(|err| anyhow::anyhow!("no renderer to photograph the display with: {err}"))?;
        crate::capture::output(&mut renderer, lxb, output)
    }

    /// Draw, small, what is on one side of the shell's own surfaces. See
    /// [`crate::capture::behind`].
    pub fn picture_behind(
        &mut self,
        lxb: &crate::state::Lxb,
        output: &Output,
        side: crate::capture::Side,
        size: smithay::utils::Size<i32, smithay::utils::Physical>,
    ) -> anyhow::Result<crate::capture::Shot> {
        let mut renderer = self
            .gpus
            .single_renderer(&self.primary_gpu)
            .map_err(|err| anyhow::anyhow!("no renderer to draw the picture with: {err}"))?;
        crate::capture::behind(&mut renderer, lxb, output, side, size)
    }
}

/// Bring up the compositor on real hardware.
pub fn init(
    event_loop: &mut EventLoop<'static, LxbState>,
    display: Display<LxbState>,
    config: Config,
    socket_name: Option<String>,
    opening: Option<crate::backdrop::Opening>,
) -> anyhow::Result<LxbState> {
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
        pointing: Vec::new(),
        dmabuf_global: None,
    }));

    let mut state = LxbState::new(
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
                // Kept as they go by, because libinput will not be asked later:
                // Settings > Input > Mouse re-applies these to the devices
                // already open, and there is no other way to find out what
                // those are. See [`UdevBackend::apply_input_settings`].
                match &event {
                    InputEvent::DeviceAdded { device } => {
                        configure_libinput_device(device, &state.lxb.config.input);
                        if device.has_capability(DeviceCapability::Pointer) {
                            if let crate::backend::Backend::Udev(udev) = &mut state.backend {
                                udev.pointing.push(device.clone());
                            }
                        }
                    }
                    InputEvent::DeviceRemoved { device } => {
                        if let crate::backend::Backend::Udev(udev) = &mut state.backend {
                            udev.pointing.retain(|had| had != device);
                        }
                    }
                    _ => {}
                }
                if is_lizard_keyboard_event(&event) {
                    return;
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
                // Reclaiming DRM master means a full modeset, which puts every
                // connector back the way the driver starts it — SDR, identity
                // colour pipeline. Nothing reports that, so the settings are
                // committed again on the frame scheduled below.
                state.lxb.hdr.reapply_all();
                // Force a fresh frame everywhere now that we own the GPU again.
                for (node, crtc) in crtcs {
                    schedule_render(state, node, crtc, Duration::ZERO);
                }
            }
        })
        .map_err(|e| anyhow::anyhow!("failed to insert session source: {e}"))?;

    // Before a single connector is lit, because lighting one commits a frame
    // and that frame is on screen: it is the first thing this session puts on a
    // display the login screen was drawing on a moment ago. See
    // [`crate::backdrop::Opening`].
    state.lxb.opening = opening;

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
                    .lxb
                    .dmabuf_state
                    .create_global_with_default_feedback::<LxbState>(
                        &state.lxb.display_handle,
                        &feedback,
                    );
                if let super::Backend::Udev(udev) = &mut state.backend {
                    udev.dmabuf_global = Some(global);
                }
            }
            Err(err) => tracing::warn!(?err, "could not build dmabuf feedback"),
        }
    }

    if state.lxb.outputs.is_empty() {
        anyhow::bail!("no usable display found; is a monitor connected?");
    }

    Ok(state)
}

/// Valve's vendor ID, and the second-generation Steam Controller's product ID.
///
/// A model number rather than anything about this machine: the same pad has the
/// same pair on every box it is plugged into.
const VALVE_VENDOR: u32 = 0x28de;
const STEAM_CONTROLLER_2: u32 = 0x1304;

/// Whether an event comes from the Steam Controller pretending to be a keyboard.
///
/// That pad has no kernel gamepad driver, so its firmware ships in *lizard
/// mode*: a real USB keyboard and mouse, where `A` is Enter, `B` is Escape and
/// the D-pad is the arrow keys. The shell reads the very same buttons out of
/// the pad's HID report, which is the only path that survives Steam claiming
/// the device — so taking both would act on every press twice.
///
/// Only the keyboard is dropped. The mouse half is the trackpad pointing, which
/// the report's decode does not replace, and it duplicates nothing.
fn is_lizard_keyboard_event(event: &InputEvent<LibinputInputBackend>) -> bool {
    let InputEvent::Keyboard { event } = event else {
        return false;
    };
    let device = event.device();
    device.id_vendor() == VALVE_VENDOR
        && device.id_product() == STEAM_CONTROLLER_2
        && device.has_capability(DeviceCapability::Keyboard)
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

/// What the primary GPU should be called, now that a device has registered its
/// renderer under a name of its own.
///
/// A card has two names and they are arrived at by different routes. The
/// primary GPU is named before any device is open, out of the DRM node alone:
/// [`renderer_node`] asks the kernel for the card's `renderD*` node. The
/// renderer is registered later, under whatever EGL says the device's render
/// node is — and EGL answers for the *driver*, so a driver with no render node
/// of its own says nothing at all and [`device_added`] falls back to the
/// primary node. That is not an exotic case: it is every software renderer,
/// which is to say most virtual machines.
///
/// When the two disagree the compositor mistakes one card for two. It looks up
/// a renderer under the startup name, nothing was ever registered under it, the
/// frame is refused — and refused again every vblank for the life of the
/// session, which on screen is one wallpaper and then nothing. The same
/// mismatch quietly withholds explicit synchronisation and the dmabuf global,
/// both of which are only offered for the primary GPU.
///
/// So the name the renderer answers to wins, because it is the name every
/// later lookup uses. Only for the card the primary GPU actually is, which is
/// either of its two names; a second card keeps its own.
///
/// Generic over the node so the decision can be tested. `DrmNode` can only be
/// built from a device that exists on the machine running the test.
fn primary_after_adding<N: Copy + PartialEq>(
    primary: N,
    node: N,
    node_render: N,
    registered: N,
) -> N {
    if node == primary || node_render == primary {
        registered
    } else {
        primary
    }
}

// ---------------------------------------------------------------------------
// device lifecycle
// ---------------------------------------------------------------------------

/// Offer explicit synchronisation, once the GPU that will import the timelines
/// is open.
///
/// Only for the primary GPU, and only once. That is the device every client's
/// buffers are imported on — see the renderer choice in [`render_surface`] —
/// so it is the one whose kernel handles a client's timeline can be resolved
/// against. A second card being plugged in later does not get a second global:
/// the protocol carries one device, and clients have already been told which.
///
/// Silently absent where the driver cannot signal a timeline point through an
/// eventfd. The protocol is deliberately not advertised then rather than
/// advertised and half-honoured, because the only other way to respect an
/// acquire point without being woken for it is to block the whole compositor
/// on the client's GPU work — which is one slow client away from a frozen
/// session.
///
/// Without this a client falls back to implicit synchronisation, which still
/// works but leaves it guessing when a buffer is free again from
/// `wl_buffer.release` alone. Every modern Vulkan client — anything through
/// DXVK or VKD3D, which is every game under Proton — expects to find this.
fn advertise_explicit_sync(
    lxb: &mut crate::state::Lxb,
    primary_gpu: DrmNode,
    render_node: DrmNode,
    fd: &DrmDeviceFd,
) {
    if lxb.syncobj_state.is_some() || render_node != primary_gpu {
        return;
    }
    if !supports_syncobj_eventfd(fd) {
        tracing::info!(
            gpu = ?render_node,
            "no explicit synchronisation: this driver cannot signal a timeline point"
        );
        return;
    }
    lxb.syncobj_state = Some(DrmSyncobjState::new::<LxbState>(
        &lxb.display_handle,
        fd.clone(),
    ));
    tracing::info!(gpu = ?render_node, "explicit synchronisation offered to clients");
}

fn device_added(state: &mut LxbState, node: DrmNode, path: &Path) -> anyhow::Result<()> {
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

    // Resetting the device to a known state means disabling every connector and
    // clearing every plane, which is a black screen for as long as it takes this
    // compositor to reach its first frame — about a fifth of a second on the
    // machine this was measured on, and it lands on top of whatever the last
    // compositor was still showing. Smithay is plain about the alternative:
    // leaving the state alone "will also prevent flickering of already turned on
    // connectors (assuming you won't change the resolution)". That is exactly
    // what a login is. The scanner then recovers the connector-to-CRTC mapping
    // the previous compositor left behind, so this one lands on the same CRTC
    // with the same mode, and its first commit is a plane update rather than a
    // modeset.
    //
    // Only where a display manager has said a hand-over is happening, though.
    // A compositor coming up on a bare TTY has no idea what left the device in
    // the state it is in, and there the known state is worth the flicker.
    let inheriting = crate::handover::wanted();
    let (drm, drm_notifier) = DrmDevice::new(fd.clone(), !inheriting)?;
    let gbm = GbmDevice::new(fd.clone())?;

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

    // The device is open, so this is the first moment anything knows what EGL
    // calls it. See [`primary_after_adding`] for why that has to be reconciled
    // with the name chosen at startup.
    let renamed = primary_after_adding(udev.primary_gpu, node, renderer_node(node), render_node);
    if renamed != udev.primary_gpu {
        tracing::info!(
            was = ?udev.primary_gpu,
            now = ?renamed,
            "primary GPU renamed to the node its renderer answers to"
        );
        udev.primary_gpu = renamed;
    }

    // Disjoint fields: `udev` holds `state.backend`, this takes `state.lxb`.
    let primary_gpu = udev.primary_gpu;
    advertise_explicit_sync(&mut state.lxb, primary_gpu, render_node, &fd);

    let registration_token = state
        .lxb
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
fn device_changed(state: &mut LxbState, node: DrmNode) {
    apply_scan(state, node);

    // Inheriting the previous compositor's configuration is what makes a login
    // seamless (see `device_added`), and it is also the one way this can be
    // handed a device it cannot use: a connector still bound to a CRTC that
    // this compositor wants for a different one fails the commit, and the
    // display is left dark. A device that came out of the scan with no surfaces
    // at all while something is plugged into it is that case, and a dark screen
    // is far worse than the flicker the inheriting was meant to avoid. Put the
    // device into the known state it would have been opened in and scan again —
    // with a fresh scanner, because the one above has already recorded these
    // connectors as seen and would report nothing the second time.
    if !inherited_an_unusable_device(state, node) {
        return;
    }
    {
        let super::Backend::Udev(udev) = &mut state.backend else {
            return;
        };
        let Some(device) = udev.devices.get_mut(&node) else {
            return;
        };
        if let Err(err) = device.drm_output_manager.device_mut().reset_state() {
            tracing::warn!(?node, ?err, "could not reset an inherited device");
            return;
        }
        device.scanner = DrmScanner::new();
    }
    tracing::info!(
        ?node,
        "inherited a display configuration this compositor cannot use; reset it and scanned again"
    );
    apply_scan(state, node);
}

/// Whether a device was handed over in a state that produced no usable output.
///
/// Only ever true on the hand-over path: a compositor that reset the device on
/// open has nothing to blame an empty scan on, and resetting it a second time
/// would not change the answer.
fn inherited_an_unusable_device(state: &LxbState, node: DrmNode) -> bool {
    let super::Backend::Udev(udev) = &state.backend else {
        return false;
    };
    let Some(device) = udev.devices.get(&node) else {
        return false;
    };
    crate::handover::wanted()
        && device.surfaces.is_empty()
        && device
            .scanner
            .connectors()
            .values()
            .any(|connector| connector.state() == connector::State::Connected)
}

/// Scan a device's connectors once and act on what changed.
fn apply_scan(state: &mut LxbState, node: DrmNode) {
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

fn device_removed(state: &mut LxbState, node: DrmNode) {
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
        state.lxb.loop_handle.remove(device.registration_token);
        tracing::info!(?node, "GPU removed");
    }
}

/// A display was plugged in (or was already there at startup).
fn connector_connected(
    state: &mut LxbState,
    node: DrmNode,
    connector: connector::Info,
    crtc: crtc::Handle,
) {
    let name = format!(
        "{}-{}",
        connector.interface().as_str(),
        connector.interface_id()
    );

    if !OutputManager::is_enabled(&name, &state.lxb.config) {
        tracing::info!(output = %name, "output disabled by config, leaving it dark");
        return;
    }

    let Some(mode) = pick_mode(
        &connector,
        state
            .lxb
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
    let global = output.create_global::<LxbState>(&state.lxb.display_handle);
    output.set_preferred(output_mode);
    output.change_current_state(Some(output_mode), None, None, None);
    OutputManager::apply_output_config(&output, &state.lxb.config);

    // The wallpaper, drawn now if this is the first display to be lit, because
    // the commit below is what establishes the mode and it goes to the screen.
    // Left to itself that commit is a frame with nothing in it, cleared to
    // black, and it is on screen until the event loop draws — a fifth of a
    // second of black laid over a login screen that was mid-animation. This
    // display's own shape, since it is the one asking. See
    // [`crate::backdrop::Opening`].
    if state.lxb.backdrop.is_none() {
        if let Some(opening) = state.lxb.opening.take() {
            state.lxb.backdrop = Some(opening.start(w as f32 / h.max(1) as f32));
        }
    }

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

    // Failing to build it is not a reason to refuse the display: the commit
    // then carries what it always carried, which is the clear colour.
    let mut opening_frame = DrmOutputRenderElements::default();
    let mut opens_on_wallpaper = false;
    if let Some(backdrop) = state.lxb.backdrop.as_ref() {
        let scale = output.current_scale().fractional_scale();
        let logical = smithay::utils::Size::<i32, smithay::utils::Logical>::from((
            (w as f64 / scale).round() as i32,
            (h as f64 / scale).round() as i32,
        ));
        match backdrop.element(&mut renderer, logical) {
            Ok(element) => {
                opening_frame.add_output(
                    &crtc,
                    state.lxb.config.general.background.into(),
                    [LxbRenderElement::Memory(element)],
                );
                opens_on_wallpaper = true;
            }
            Err(err) => {
                tracing::warn!(?err, output = %name, "could not draw the wallpaper this display comes up in")
            }
        }
    }

    let drm_output = match device
        .drm_output_manager
        .initialize_output::<_, LxbRenderElement<UdevRenderer<'_>>>(
            crtc,
            mode,
            &[connector.handle()],
            &output,
            None,
            &mut renderer,
            &opening_frame,
        ) {
        Ok(drm_output) => drm_output,
        Err(err) => {
            tracing::warn!(?err, output = %name, "failed to initialise output");
            return;
        }
    };
    drop(renderer);

    // What this connector can be driven at, before anything is drawn on it.
    // Probed here rather than on the first request because both halves of the
    // answer are fixed for as long as the cable is in: the display's EDID, and
    // which of the five properties this driver publishes.
    let drm_device = device.drm_output_manager.device();
    let hdr_display = crate::hdr::Display::probe(drm_device, connector.handle());
    let hdr =
        crate::hdr::Pipeline::probe(drm_device, connector.handle(), crtc, drm_device.is_atomic());
    let hdr_status = crate::hdr::Status {
        supported: hdr.supported() && hdr_display.st2084,
        enabled: false,
        max_luminance: hdr_display.max_luminance.unwrap_or(0),
        gamut: hdr.converts_gamut(),
        // And whether it is the exact conversion or the approximation of it,
        // which is a different question about the same matrix — see
        // `Pipeline::converts_gamut_exactly`.
        gamut_exact: hdr.converts_gamut_exactly(),
        // A much shorter question than the one above it: a gamma ramp and an
        // atomic commit, with nothing asked of the display or the link. See
        // `Pipeline::warms`.
        night_light: hdr.warms(),
        warming: false,
    };

    device.surfaces.insert(
        crtc,
        SurfaceData {
            output: output.clone(),
            global: Some(global),
            drm_output,
            render_state: RenderState::Idle,
            queued_at: None,
            connector: connector.handle(),
            modes: connector.modes().to_vec(),
            hdr,
            hdr_display,
        },
    );

    let config = state.lxb.config.clone();
    state
        .lxb
        .outputs
        .add_output(&mut state.lxb.space, &output, &config);
    // Registering marks it pending, so whatever the config asks for is applied
    // on this display's first frame without a separate startup path.
    state.lxb.hdr.register(
        &output,
        config.hdr_for(&name),
        config.night_light_for(&name),
        hdr_status,
    );

    tracing::info!(
        output = %name,
        width = w,
        height = h,
        refresh_mhz = refresh,
        modes = connector.modes().len(),
        hdr = hdr_status.supported,
        peak_nits = hdr_status.max_luminance,
        night_light = hdr_status.night_light,
        // What the commit above put on this display — which is what the user
        // is looking at until the event loop draws. See `crate::backdrop`.
        opened_on = if opens_on_wallpaper {
            "the wallpaper"
        } else {
            "the clear colour"
        },
        "display connected"
    );

    // What this display can be driven at, how its picture is turned, and where
    // it stands among the others are pages in the shell's Settings column, and
    // a display that has just arrived has to appear in them without waiting for
    // something else on screen to change. The last of the three is also about
    // the displays that were already here: a screen arriving takes the place at
    // the end of the row, and the page has to say so on every one of them.
    state.refresh_modes();
    state.refresh_transforms();
    state.refresh_places();

    schedule_render(state, node, crtc, Duration::ZERO);
}

fn connector_disconnected(
    state: &mut LxbState,
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

fn remove_surface(state: &mut LxbState, node: DrmNode, crtc: crtc::Handle) {
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

    let config = state.lxb.config.clone();
    state
        .lxb
        .outputs
        .remove_output(&mut state.lxb.space, &surface.output, &config);
    // What it was set to survives; what it can do does not, until it is back.
    state.lxb.hdr.disconnected(&surface.output);
    // And nothing can be recorded off a connector that is no longer there, so
    // whoever was waiting on a frame of it is told rather than left waiting.
    state.lxb.screencopy.output_gone(&surface.output);
    // Nor can it be driven at anything, or turned, until then — which the
    // shell's pages have to hear about the same way they heard it arrive. Its
    // leaving renumbers every display that was laid out after it, so the
    // arrangement is republished for the survivors as well.
    state.refresh_modes();
    state.refresh_transforms();
    state.refresh_places();

    if let Some(global) = surface.global {
        state.lxb.display_handle.remove_global::<LxbState>(global);
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
        if let Some(mode) = match_mode(modes, want) {
            return Some(mode);
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

/// The mode in `modes` a request names, if any of them is that size.
///
/// The size has to be exact — a display asked for 1080p and given 720p has
/// been given the wrong thing — but the refresh is matched to the nearest,
/// which is what lets `@60` name a 59.94 Hz mode. Without a refresh the
/// fastest of that size wins, since a resolution asked for on its own is asked
/// for at its best.
fn match_mode(
    modes: &[smithay::reexports::drm::control::Mode],
    want: ModeRequest,
) -> Option<smithay::reexports::drm::control::Mode> {
    let mut matching: Vec<_> = modes
        .iter()
        .filter(|m| {
            let (w, h) = m.size();
            w as i32 == want.width && h as i32 == want.height
        })
        .collect();

    match want.refresh {
        Some(refresh) => matching.sort_by_key(|m| (OutputMode::from(**m).refresh - refresh).abs()),
        None => matching.sort_by_key(|m| std::cmp::Reverse(OutputMode::from(**m).refresh)),
    }
    matching.first().map(|mode| **mode)
}

/// Every mode one display can be driven at, as the shell is told about them.
///
/// Empty for an output this backend is not driving, which is the same answer a
/// nested backend gives for all of them: there is no connector, so there is
/// nothing to choose between.
pub fn output_modes(state: &LxbState, output: &Output) -> Vec<DisplayMode> {
    let super::Backend::Udev(udev) = &state.backend else {
        return Vec::new();
    };
    let Some(surface) = udev
        .devices
        .values()
        .flat_map(|device| device.surfaces.values())
        .find(|surface| &surface.output == output)
    else {
        return Vec::new();
    };

    let current = output.current_mode();
    let mut listed: Vec<DisplayMode> = Vec::with_capacity(surface.modes.len());
    for mode in &surface.modes {
        let size = OutputMode::from(*mode);
        let entry = DisplayMode {
            width: size.size.w.max(0) as u32,
            height: size.size.h.max(0) as u32,
            refresh: size.refresh.max(0) as u32,
            current: current == Some(size),
            preferred: mode.mode_type().contains(ModeTypeFlags::PREFERRED),
        };
        // A connector may list the same resolution and rate several times over
        // — different timings for the same picture — and two rows a shell
        // cannot tell apart are one choice presented twice.
        if let Some(seen) = listed.iter_mut().find(|seen| {
            seen.width == entry.width
                && seen.height == entry.height
                && seen.refresh == entry.refresh
        }) {
            seen.current |= entry.current;
            seen.preferred |= entry.preferred;
            continue;
        }
        listed.push(entry);
    }
    listed
}

/// Drive a display at a different mode. `true` when the hardware took it.
///
/// The whole display is reconfigured around it: every window is re-tiled, the
/// layer surfaces — the shell's own bar among them — are re-arranged to the
/// new size, and the outputs beside it are re-packed, because a display that
/// changes width moves the ones laid out after it.
pub fn set_output_mode(state: &mut LxbState, output: &Output, want: ModeRequest) -> bool {
    let Some((node, crtc, mode)) = ({
        let super::Backend::Udev(udev) = &state.backend else {
            return false;
        };
        udev.devices
            .iter()
            .flat_map(|(node, device)| {
                device
                    .surfaces
                    .iter()
                    .map(move |(crtc, surface)| (*node, *crtc, surface))
            })
            .find(|(_, _, surface)| &surface.output == output)
            .and_then(|(node, crtc, surface)| Some((node, crtc, match_mode(&surface.modes, want)?)))
    }) else {
        tracing::info!(
            output = %output.name(),
            width = want.width,
            height = want.height,
            "no such mode on this display; it keeps the one it has"
        );
        return false;
    };

    let wanted = OutputMode::from(mode);
    if output.current_mode() == Some(wanted) {
        return false;
    }

    {
        let super::Backend::Udev(udev) = &mut state.backend else {
            return false;
        };
        let Some(device) = udev.devices.get_mut(&node) else {
            return false;
        };
        let render_node = device.render_node;
        let mut renderer = match udev.gpus.single_renderer(&render_node) {
            Ok(renderer) => renderer,
            Err(err) => {
                tracing::warn!(?err, output = %output.name(), "no renderer to change the mode with");
                return false;
            }
        };
        let Some(surface) = device.surfaces.get_mut(&crtc) else {
            return false;
        };
        if let Err(err) = surface
            .drm_output
            .use_mode::<_, LxbRenderElement<UdevRenderer<'_>>>(
                mode,
                &mut renderer,
                &DrmOutputRenderElements::default(),
            )
        {
            tracing::warn!(?err, output = %output.name(), "the display would not take that mode");
            return false;
        }
    }

    // Only now, once the hardware has it: an output advertising a mode the
    // CRTC refused would have every client drawing at a size nothing scans out.
    output.change_current_state(Some(wanted), None, None, None);

    // The bar and anything else anchored to this display are sized from the
    // output's geometry, so they have to be re-arranged before the windows are
    // tiled into what is left over.
    smithay::desktop::layer_map_for_output(output).arrange();
    let config = state.lxb.config.clone();
    state.lxb.outputs.relayout(&mut state.lxb.space, &config);

    // A mode set is a modeset, and it leaves the connector the way the driver
    // starts it — colorimetry, metadata and colour pipeline included. Nothing
    // reports that, so a display in HDR would come back washed out with the
    // Settings column still saying it was on.
    state.lxb.hdr.reapply(output);

    tracing::info!(
        output = %output.name(),
        width = wanted.size.w,
        height = wanted.size.h,
        refresh_mhz = wanted.refresh,
        "driving this display at a new mode"
    );

    // What the display is now doing, for the page that asked.
    state.refresh_modes();
    schedule_render(state, node, crtc, Duration::ZERO);
    true
}

// ---------------------------------------------------------------------------
// rendering
// ---------------------------------------------------------------------------

fn schedule_render(state: &mut LxbState, node: DrmNode, crtc: crtc::Handle, delay: Duration) {
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
        .lxb
        .loop_handle
        .insert_source(timer, move |_, _, state| {
            render_surface(state, node, crtc);
            TimeoutAction::Drop
        });
    if let Err(err) = res {
        tracing::warn!(?err, "failed to schedule a render");
    }
}

fn render_surface(state: &mut LxbState, node: DrmNode, crtc: crtc::Handle) {
    // Before the borrows below, because committing HDR needs the whole state:
    // the settings are the session's and the properties are the device's. Here
    // rather than where the request arrives because *here* is the one moment
    // this connector is quiet — the last frame has been scanned out and the
    // next has not been queued — and a property commit racing a page flip on
    // the same CRTC comes back EBUSY.
    apply_pending_hdr(state, node, crtc);

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

    udev.cursor.status = state.lxb.cursor_now();
    let draw_cursor = state.lxb.config.general.draw_cursor;
    let clear_color = state.lxb.config.general.background;

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

    // Which is also the moment this frame's curtain was decided: read before
    // the elements are built, so a frame counted as black had the black over
    // every element in it. See [`crate::curtain::Curtain::a_frame_was_drawn`].
    let frame_started = std::time::Instant::now();
    let elements = output_elements(
        &mut renderer,
        &state.lxb,
        &output,
        draw_cursor.then_some(&mut udev.cursor),
    );

    let refresh = output
        .current_mode()
        .map(|m| Duration::from_secs_f64(1000.0 / m.refresh as f64))
        .unwrap_or(Duration::from_millis(16));

    // Decided per frame rather than when the client asks, because what makes a
    // frame safe to tear is what is on the screen with it, and that changes
    // between one frame and the next — the guide opening over a game is the
    // whole case. See [`crate::render::output_may_tear`].
    let may_tear = crate::render::output_may_tear(&state.lxb, &output);
    // Asked here rather than where the client speaks, for the reason tearing
    // is: what makes a frame safe to pass through is everything *else* on the
    // screen, and that changes between one frame and the next. Recorded now
    // and committed by `apply_pending_hdr` at the next quiet moment, because a
    // property commit racing a page flip on the same CRTC comes back EBUSY.
    let encoded = crate::render::output_shows_encoded_content(&state.lxb, &output);
    if state.lxb.hdr.request_passthrough(&output, encoded) {
        tracing::info!(
            display = %output.name(),
            passthrough = encoded,
            "the colour pipeline is following what is on screen"
        );
    }
    surface.drm_output.with_compositor(|compositor| {
        compositor.surface().set_tearing(may_tear);
    });

    let result =
        surface
            .drm_output
            .render_frame(&mut renderer, &elements, clear_color, FrameFlags::DEFAULT);

    drop(renderer);

    let time = state.lxb.start_time.elapsed();
    match result {
        Ok(render_result) => {
            // Before the feedback below is collected, and before the frame is
            // queued: this is what records where each surface was drawn, and
            // both of those read it back. See
            // [`crate::render::record_where_each_surface_was_drawn`].
            post_repaint(&state.lxb, &output, time, None, &render_result.states);
            // And, if this frame was a black one, the display it puts the
            // black on. Whether it is *queued* below makes no difference:
            // a frame nothing changed in is one the screen is already showing.
            state.lxb.curtain.a_frame_was_drawn(&output, frame_started);

            let feedback = if render_result.is_empty {
                None
            } else {
                Some(crate::render::collect_presentation_feedback(
                    &state.lxb,
                    &output,
                    &render_result.states,
                ))
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
                        surface.queued_at = Some(std::time::Instant::now());
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
            } else {
                watch_for_a_lost_flip(state, node, crtc);
            }
        }
        Err(err) => {
            tracing::warn!(?err, output = output.name(), "failed to render frame");
            schedule_render(state, node, crtc, refresh);
            // Nothing was drawn, so nothing was drawn anywhere — but the
            // clients still have to be paced, or a failed frame would stop
            // every one of them for good.
            post_repaint(
                &state.lxb,
                &output,
                time,
                None,
                &RenderElementStates::default(),
            );
        }
    }

    serve_screencopy(state, &output, time);
}

/// Say so if the frame just queued never reaches the screen.
///
/// Every frame this compositor draws is queued as a page flip and answered by a
/// vblank event, and the next frame is only drawn once that answer arrives. So
/// an answer that never comes is not a dropped frame: it is the end of this
/// display. It never draws again, every client on it stops being sent frame
/// callbacks and blocks in its own present, and nothing anywhere says why —
/// the picture simply stops, still showing the last frame that made it.
///
/// A second is several dozen refreshes. Nothing legitimate takes that long, and
/// the check costs one timer per frame that is already waiting on the kernel.
fn watch_for_a_lost_flip(state: &mut LxbState, node: DrmNode, crtc: crtc::Handle) {
    let queued_at = {
        let super::Backend::Udev(udev) = &state.backend else {
            return;
        };
        let Some(surface) = udev.devices.get(&node).and_then(|d| d.surfaces.get(&crtc)) else {
            return;
        };
        surface.queued_at
    };
    let timer = Timer::from_duration(Duration::from_secs(1));
    let res = state
        .lxb
        .loop_handle
        .insert_source(timer, move |_, _, state| {
            let super::Backend::Udev(udev) = &state.backend else {
                return TimeoutAction::Drop;
            };
            let Some(surface) = udev.devices.get(&node).and_then(|d| d.surfaces.get(&crtc)) else {
                return TimeoutAction::Drop;
            };
            // The same frame, still waiting: `queued_at` is what tells this
            // from the ordinary case of several frames having gone by since.
            if surface.queued_at == queued_at
                && matches!(surface.render_state, RenderState::WaitingForVblank { .. })
            {
                tracing::error!(
                    output = %surface.output.name(),
                    waited = ?queued_at.map(|at| at.elapsed()),
                    "a frame was queued and the display never answered; nothing on this screen \
                     will be drawn or paced again"
                );
            }
            TimeoutAction::Drop
        });
    if let Err(err) = res {
        tracing::warn!(?err, "could not watch for a lost page flip");
    }
}

/// Answer whatever is recording this display, now that its frame has been
/// drawn.
///
/// On the primary GPU, for the reason a screenshot is taken there: the picture
/// is composited into a buffer of our own rather than scanned out, and the
/// primary is where every client's own buffers are importable.
fn serve_screencopy(state: &mut LxbState, output: &Output, time: Duration) {
    if !state.lxb.screencopy.wanted(output) {
        return;
    }
    let LxbState { backend, lxb } = state;
    let super::Backend::Udev(udev) = backend else {
        return;
    };
    let UdevBackend {
        gpus,
        cursor,
        primary_gpu,
        ..
    } = &mut **udev;
    let mut renderer = match gpus.single_renderer(primary_gpu) {
        Ok(renderer) => renderer,
        Err(err) => {
            tracing::warn!(?err, display = %output.name(), "no renderer to copy the screen with");
            return;
        }
    };
    let cursor = lxb.config.general.draw_cursor.then_some(cursor);
    crate::screencopy::serve(&mut renderer, lxb, output, cursor, time);
}

/// Put a display's colour pipeline into force, if it is waiting for any of it:
/// its HDR settings and its night light, which are one commit.
///
/// The connector's own properties are set out of band rather than folded into
/// the frame smithay is about to queue: none of the five is part of the atomic
/// state smithay's surface models, so this is the only way to reach them
/// without a second atomic request fighting the first over the same CRTC.
fn apply_pending_hdr(state: &mut LxbState, node: DrmNode, crtc: crtc::Handle) {
    let super::Backend::Udev(udev) = &mut state.backend else {
        return;
    };
    let Some(device) = udev.devices.get_mut(&node) else {
        return;
    };
    let Some(surface) = device.surfaces.get_mut(&crtc) else {
        return;
    };
    let output = surface.output.clone();
    let Some((settings, night, passthrough)) = state.lxb.hdr.take_pending(&output) else {
        return;
    };

    let applied = surface.hdr.apply(
        device.drm_output_manager.device(),
        surface.connector,
        crtc,
        &surface.hdr_display,
        &settings,
        &night,
        passthrough,
    );
    let enabled = applied.enabled;

    // Every property in that request can force a modeset, and several of them
    // do: the driver tears the pipe down and builds it again. Smithay committed
    // the frame that is on screen and has heard nothing since, so it still
    // believes its own state is what the CRTC is in — and the next render, with
    // nothing on screen having changed, would come back empty and queue no
    // flip at all. The display would then hold whatever the modeset left it
    // with, which is a black one, for as long as nothing dirtied that output.
    //
    // This is the same thing smithay does for itself on VT switch, for the same
    // reason: re-read the CRTC and refuse to call the next frame empty, so the
    // render below submits a real one and the pipe is armed again.
    if applied.committed {
        if let Err(err) = surface
            .drm_output
            .with_compositor(|compositor| compositor.reset_state())
        {
            tracing::warn!(
                ?err,
                output = %output.name(),
                "could not re-read the CRTC after a colour pipeline commit"
            );
        }
    }

    let pipeline = surface.hdr.describe();
    let before = state.lxb.hdr.status(&output);
    let was_on = before.enabled;

    // Said on every applied change rather than only on the ones that flip the
    // switch: turning the white level up is as visible as turning HDR on, and
    // a log that only recorded the switch would leave a session that came out
    // too dark with nothing to read.
    if enabled {
        tracing::info!(
            output = %output.name(),
            white_nits = settings.sdr_brightness,
            srgb_intensity = settings.srgb_intensity,
            peak_nits = settings.peak_brightness.unwrap_or(0),
            pipeline,
            "driving this display in HDR"
        );
    } else if settings.enabled {
        tracing::warn!(
            output = %output.name(),
            "this display cannot be driven in HDR; it stays in SDR"
        );
    } else if was_on {
        tracing::info!(output = %output.name(), "this display is back in SDR");
    }

    // The night light on its own line, and only when it moves: it is a
    // separate decision from HDR, it is the one of the two that changes itself
    // twice a day on a schedule, and a session whose evening never arrived
    // needs that in the log without a colour pipeline dump around it.
    if applied.warming != before.warming {
        match applied.warming {
            true => tracing::info!(
                output = %output.name(),
                kelvin = night.temperature,
                "warming this display's picture"
            ),
            false => tracing::info!(output = %output.name(), "this display is back to daylight"),
        }
    } else if night.tints() && !applied.warming && applied.committed {
        tracing::warn!(
            output = %output.name(),
            "this display has no gamma ramp to warm; its picture is unfiltered"
        );
    }

    // The shell showed the setting as taking effect; tell it what actually
    // did. Nothing else notices a status change.
    if state.lxb.hdr.applied(&output, applied) {
        state.refresh_hdr();
    }
}

fn on_vblank(
    state: &mut LxbState,
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
                    _ => state.lxb.start_time.elapsed(),
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

/// Put every display back into SDR, before the session gives up DRM master.
///
/// Nothing else does this. Dropping the master restores the console's *mode*,
/// but not the connector's colorimetry or the CRTC's colour pipeline, so a
/// session that exits while a display is in BT.2020/PQ hands the user back a
/// TTY encoded for a transfer function the console knows nothing about — dark,
/// wrong, and with no obvious way to put it right short of a reboot. Whoever
/// turned HDR on for a session is entitled to get their console back when it
/// ends.
pub fn restore_displays(state: &mut LxbState) {
    let super::Backend::Udev(udev) = &mut state.backend else {
        return;
    };
    // Read before the loop below borrows the backend: the warmth each display
    // is under is kept across the hand-over, and it lives in the state the
    // loop cannot reach from inside.
    let warmth: Vec<(crtc::Handle, crate::hdr::NightLight)> = udev
        .devices
        .values()
        .flat_map(|device| device.surfaces.iter())
        .map(|(crtc, surface)| (*crtc, state.lxb.hdr.night_light_of(&surface.output)))
        .collect();
    for device in udev.devices.values_mut() {
        // Split so the device is not borrowed twice: the surfaces are what
        // hold the pipelines, and the manager underneath them is the DRM
        // device every commit goes to.
        let drm = device.drm_output_manager.device();
        for (crtc, surface) in device.surfaces.iter_mut() {
            let night = warmth
                .iter()
                .find(|(warmed, _)| warmed == crtc)
                .map(|(_, night)| *night)
                .unwrap_or_default();
            surface.hdr.reset(drm, surface.connector, *crtc, &night);
        }
    }
}

/// Leave the picture on every display for the compositor that follows.
///
/// The counterpart to [`restore_displays`], and called instead of it: rather
/// than putting the displays back the way a console would want them, this hands
/// them on exactly as they are and forks a keeper to stop the kernel taking
/// them down behind us. Answers how many displays that keeper is watching.
///
/// See [`crate::handover`] for what the kernel does otherwise, and why the
/// answer is a file descriptor rather than anything either compositor commits.
pub fn hold_displays(state: &LxbState) -> usize {
    let super::Backend::Udev(udev) = &state.backend else {
        return 0;
    };
    let displays: Vec<(RawFd, u32)> = udev
        .devices
        .values()
        .flat_map(|device| {
            let fd = device.drm_output_manager.device().as_fd().as_raw_fd();
            device
                .surfaces
                .keys()
                .map(move |crtc| (fd, u32::from(*crtc)))
        })
        .collect();
    crate::handover::hold(&displays)
}

/// Ask every output to draw again. Called when compositor state changes.
pub fn queue_redraw_all(state: &mut LxbState) {
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

#[cfg(test)]
mod tests {
    use super::primary_after_adding;

    /// A card answers to one name, whichever route arrived at it.
    ///
    /// The middle case is the one that took a session away: `card1` resolves to
    /// `renderD128` before anything is open, then EGL declines to name a render
    /// node — every software renderer — and the renderer is registered under
    /// `card1` after all. Two names, one card, and a compositor that draws one
    /// frame and then refuses every vblank for the rest of the session.
    #[test]
    fn one_card_does_not_become_two() {
        // Named `renderD128` at startup, registered under `card1`.
        assert_eq!(
            primary_after_adding("renderD128", "card1", "renderD128", "card1"),
            "card1"
        );
        // Named `card1` at startup, registered under `renderD128`.
        assert_eq!(
            primary_after_adding("card1", "card1", "renderD128", "renderD128"),
            "renderD128"
        );
        // Both routes already agree, which is every machine with a real driver.
        assert_eq!(
            primary_after_adding("renderD128", "card1", "renderD128", "renderD128"),
            "renderD128"
        );
        // A second card is not the primary one and does not rename it.
        assert_eq!(
            primary_after_adding("renderD128", "card2", "renderD129", "renderD129"),
            "renderD128"
        );
    }
}
