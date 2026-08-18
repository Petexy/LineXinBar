//! `frog_color_management_v1`: letting a game say its pixels are HDR.
//!
//! Everything LineXinBar composites is SDR, and [`crate::hdr`] is about the
//! *display* — what the user chose for it in Settings > Display, carried out
//! against the connector's own colour pipeline. Nothing in that path lets a
//! client say anything at all, so a game rendering ST 2084 handed its frames
//! over and had them shown as though they were sRGB: grey, flat, and wrong in
//! a way that looks like a broken game rather than a missing protocol.
//!
//! This is the interface that gap is normally closed with. It is not in
//! wayland-protocols — it is Valve's, and it is what their HDR Vulkan layer
//! and Gamescope speak, which is exactly what makes it the one a game under
//! Proton will look for. `ENABLE_HDR_WSI=1` is a request for this global.
//!
//! ## What this answers, and why it is sRGB
//!
//! Every client is told the output is sRGB — including on a display this
//! session is driving in HDR. That is the honest answer rather than a
//! placeholder, and the reason is [`crate::hdr`]'s pipeline:
//!
//! ```text
//! framebuffer -> DEGAMMA_LUT -> CTM -> GAMMA_LUT -> connector
//!  sRGB-coded     linear light   BT.2020   PQ-coded
//! ```
//!
//! HDR here means the *display* is put into PQ and the CRTC re-encodes an
//! sRGB-coded framebuffer into it. That transform belongs to the display and
//! reaches everything scanned out. So a client told "PQ" would send PQ pixels,
//! they would be read as sRGB, and they would be encoded a second time —
//! blown out, and considerably worse than the flat picture this protocol
//! exists to fix. Answering sRGB has the client render sRGB, which is what
//! this compositor can show correctly today.
//!
//! What that buys is still worth having: a client asking for HDR now gets a
//! definite answer instead of silence, so `ENABLE_HDR_WSI=1` stops being a
//! coin toss, and the panel's peak luminance is reported truthfully.
//!
//! ## What is left to do
//!
//! Passthrough. For a client's own PQ pixels to reach the display, the
//! pipeline above has to be set to identity for the frames a colour-managed
//! surface owns the display outright — connector in BT.2020/PQ, DEGAMMA, CTM
//! and GAMMA all doing nothing — so the client's encoding is the one that
//! arrives. That is a change in [`crate::hdr`], not here, and it brings a
//! second problem with it: the shell drawn over such a frame is SDR, and SDR
//! numbers read as PQ are far too dark. Gamescope answers that by tone-mapping
//! its overlay. Until both are solved, reporting sRGB is correct rather than
//! cautious.
//!
//! The chromaticities below are already in the form that work needs: BT.2020
//! with a D65 white point is what an HDR display would be described as, and
//! Rec.709 is what it is described as now.

use std::sync::{Arc, Mutex};

use lxb_protocol::server::frog::frog_color_managed_surface::{
    self, FrogColorManagedSurface, Primaries, TransferFunction,
};
use lxb_protocol::server::frog::frog_color_management_factory_v1::{
    self, FrogColorManagementFactoryV1,
};
use smithay::output::Output;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New,
};
use smithay::utils::IsAlive;
use smithay::wayland::compositor::with_states;

/// BT.2020's primaries and the D65 white point, in the protocol's units of
/// 0.00002 — the same encoding CTA-861 mastering metadata uses, where 50000 is
/// 1.0.
const BT2020: Chromaticity = Chromaticity {
    red: (35400, 14600),
    green: (8500, 39850),
    blue: (6550, 2300),
    white: (15635, 16450),
};

/// Rec.709's, which is sRGB's, in the same units. What an SDR display is
/// described as.
const REC709: Chromaticity = Chromaticity {
    red: (32000, 16500),
    green: (15000, 30000),
    blue: (7500, 3000),
    white: (15635, 16450),
};

#[derive(Debug, Clone, Copy)]
struct Chromaticity {
    red: (u32, u32),
    green: (u32, u32),
    blue: (u32, u32),
    white: (u32, u32),
}

/// What one surface has said about its own pixels.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct SurfaceColour {
    pub transfer: Option<TransferFunction>,
    pub primaries: Option<Primaries>,
}

impl SurfaceColour {
    /// Whether this describes high dynamic range content — the only question
    /// the display pipeline actually acts on.
    ///
    /// PQ is the one that matters. scRGB linear is HDR too, but LineXinBar
    /// composites in 8-bit sRGB and has nowhere to put its extended range, so
    /// claiming to honour it would be the half-honouring this module exists to
    /// avoid.
    pub fn is_hdr(&self) -> bool {
        matches!(self.transfer, Some(TransferFunction::St2084Pq))
    }
}

/// Per-`frog_color_managed_surface` state.
#[derive(Debug)]
pub struct ColourSurfaceData {
    surface: WlSurface,
    colour: Mutex<SurfaceColour>,
}

/// What `surface` has said about its colour, if anything.
pub fn surface_colour(surface: &WlSurface) -> SurfaceColour {
    with_states(surface, |states| {
        states
            .data_map
            .get::<Mutex<SurfaceColour>>()
            .map(|colour| *colour.lock().unwrap())
            .unwrap_or_default()
    })
}

/// The global.
#[derive(Debug)]
pub struct ColourState {
    _global: smithay::reexports::wayland_server::backend::GlobalId,
}

impl ColourState {
    pub fn new<D>(display: &DisplayHandle) -> Self
    where
        D: GlobalDispatch<FrogColorManagementFactoryV1, ()>
            + Dispatch<FrogColorManagementFactoryV1, ()>
            + Dispatch<FrogColorManagedSurface, Arc<ColourSurfaceData>>
            + 'static,
    {
        Self {
            _global: display.create_global::<D, FrogColorManagementFactoryV1, _>(1, ()),
        }
    }
}

/// Tell one colour-managed surface what the display it is on can do.
///
/// `hdr` is whether that display is *being driven* in HDR, not whether it could
/// be: a client is told what its pixels will actually meet.
pub fn send_preferred_metadata(
    resource: &FrogColorManagedSurface,
    hdr: bool,
    max_luminance: u16,
    min_luminance: f32,
) {
    let (transfer, volume) = if hdr {
        (TransferFunction::St2084Pq, BT2020)
    } else {
        (TransferFunction::Srgb, REC709)
    };
    resource.preferred_metadata(
        transfer,
        volume.red.0,
        volume.red.1,
        volume.green.0,
        volume.green.1,
        volume.blue.0,
        volume.blue.1,
        volume.white.0,
        volume.white.1,
        u32::from(max_luminance),
        // The protocol carries the black level in the same units the HDR
        // infoframe does: ten-thousandths of a candela.
        (min_luminance * 10_000.0).round().max(0.0) as u32,
        u32::from(max_luminance),
    );
}

impl<D> GlobalDispatch<FrogColorManagementFactoryV1, (), D> for ColourState
where
    D: GlobalDispatch<FrogColorManagementFactoryV1, ()>
        + Dispatch<FrogColorManagementFactoryV1, ()>,
{
    fn bind(
        _state: &mut D,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<FrogColorManagementFactoryV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, D>,
    ) {
        data_init.init(resource, ());
    }
}

impl<D> Dispatch<FrogColorManagementFactoryV1, (), D> for ColourState
where
    D: Dispatch<FrogColorManagementFactoryV1, ()>
        + Dispatch<FrogColorManagedSurface, Arc<ColourSurfaceData>>
        + ColourHandler
        + 'static,
{
    fn request(
        state: &mut D,
        _client: &Client,
        _factory: &FrogColorManagementFactoryV1,
        request: frog_color_management_factory_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            frog_color_management_factory_v1::Request::GetColorManagedSurface {
                surface,
                callback,
            } => {
                with_states(&surface, |states| {
                    states
                        .data_map
                        .insert_if_missing_threadsafe(|| Mutex::new(SurfaceColour::default()));
                });
                let resource = data_init.init(
                    callback,
                    Arc::new(ColourSurfaceData {
                        surface: surface.clone(),
                        colour: Mutex::new(SurfaceColour::default()),
                    }),
                );
                // A client asks this first and decides what to render from the
                // answer, so it must not have to wait for a frame to hear it.
                state.describe_surface(&surface, &resource);
            }
            frog_color_management_factory_v1::Request::Destroy => {}
            _ => {}
        }
    }
}

impl<D> Dispatch<FrogColorManagedSurface, Arc<ColourSurfaceData>, D> for ColourState
where
    D: Dispatch<FrogColorManagedSurface, Arc<ColourSurfaceData>> + ColourHandler + 'static,
{
    fn request(
        state: &mut D,
        _client: &Client,
        _resource: &FrogColorManagedSurface,
        request: frog_color_managed_surface::Request,
        data: &Arc<ColourSurfaceData>,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        let mut colour = *data.colour.lock().unwrap();
        match request {
            frog_color_managed_surface::Request::SetKnownTransferFunction { transfer_function } => {
                colour.transfer = transfer_function.into_result().ok();
            }
            frog_color_managed_surface::Request::SetKnownContainerColorVolume { primaries } => {
                colour.primaries = primaries.into_result().ok();
            }
            // Accepted and deliberately not acted on. The render intent is
            // about how a colour outside the output's gamut should be brought
            // inside it, and nothing here converts gamuts per surface; the
            // pipeline in `crate::hdr` is the display's, not the surface's.
            frog_color_managed_surface::Request::SetRenderIntent { .. } => return,
            // Likewise the mastering metadata. It describes the display the
            // content was graded on, which is only useful to a compositor that
            // tone-maps between that and this one's — and tone-mapping a
            // client's pixels is the thing this module does not do.
            frog_color_managed_surface::Request::SetHdrMetadata { .. } => return,
            frog_color_managed_surface::Request::Destroy => {
                colour = SurfaceColour::default();
                store(&data.surface, colour);
                *data.colour.lock().unwrap() = colour;
                state.colour_changed(&data.surface);
                return;
            }
            _ => return,
        }
        *data.colour.lock().unwrap() = colour;
        store(&data.surface, colour);
        state.colour_changed(&data.surface);
    }
}

/// Record what a surface has said about its colour.
///
/// Public because [`crate::colour_management`] writes the same slot: a surface
/// has one colour whichever of the two protocols described it, and everything
/// downstream reads it through [`surface_colour`] without knowing which spoke.
pub fn set_surface_colour(surface: &WlSurface, colour: SurfaceColour) {
    store(surface, colour);
}

fn store(surface: &WlSurface, colour: SurfaceColour) {
    if !surface.alive() {
        return;
    }
    with_states(surface, |states| {
        states
            .data_map
            .insert_if_missing_threadsafe(|| Mutex::new(SurfaceColour::default()));
        if let Some(slot) = states.data_map.get::<Mutex<SurfaceColour>>() {
            *slot.lock().unwrap() = colour;
        }
    });
}

/// What the compositor has to supply for the protocol to mean anything.
pub trait ColourHandler {
    /// A surface has changed what it says about its colour, so the display it
    /// is on may need to change with it.
    fn colour_changed(&mut self, surface: &WlSurface);
    /// Tell one surface what the display it is on is being driven as.
    fn describe_surface(&mut self, surface: &WlSurface, resource: &FrogColorManagedSurface);
    /// The display a surface is on, if it is on one.
    fn output_of(&self, surface: &WlSurface) -> Option<Output>;
}

#[macro_export]
macro_rules! delegate_colour_management {
    ($ty:ty) => {
        smithay::reexports::wayland_server::delegate_global_dispatch!($ty: [
            lxb_protocol::server::frog::frog_color_management_factory_v1::FrogColorManagementFactoryV1: ()
        ] => $crate::colour::ColourState);
        smithay::reexports::wayland_server::delegate_dispatch!($ty: [
            lxb_protocol::server::frog::frog_color_management_factory_v1::FrogColorManagementFactoryV1: ()
        ] => $crate::colour::ColourState);
        smithay::reexports::wayland_server::delegate_dispatch!($ty: [
            lxb_protocol::server::frog::frog_color_managed_surface::FrogColorManagedSurface: std::sync::Arc<$crate::colour::ColourSurfaceData>
        ] => $crate::colour::ColourState);
    };
}
