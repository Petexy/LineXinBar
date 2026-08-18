//! `wp_color_manager_v1`: the same question [`crate::colour`] answers, asked
//! the standard way.
//!
//! Valve's `frog_color_management_v1` is what a game under Proton looks for,
//! and it is what this compositor answered first. It is not what anything else
//! speaks. Firefox — and every fork of it, Zen included — implements this
//! protocol and only this one, so under LineXinBar a browser had no way to
//! learn what its pixels would meet and no way to say what they were. Turning
//! HDR on in `about:config` therefore could not work: `gfx.color_management.hdr`
//! finds no colour manager, and the `force_enabled` companion pref makes Gecko
//! render high dynamic range anyway, blind, into buffers this compositor then
//! reads as sRGB.
//!
//! This module closes that. It is deliberately the *same* answer as the frog
//! one — the two protocols write into one [`SurfaceColour`] per surface, and
//! [`crate::render::output_shows_encoded_content`] cannot tell which of them
//! spoke. There is one HDR pipeline here and it belongs to the display; adding
//! a second way to ask about it must not add a second way for it to behave.
//!
//! ## What is advertised, and what is left out
//!
//! Every entry in `supported_feature`, `supported_tf_named` and
//! `supported_primaries_named` is a promise that the matching request will be
//! *accepted*. Claim too few and a client that assumed one gets a protocol
//! error and dies, which is worse than the silence this replaces. Claim too
//! many and it renders something that cannot be shown correctly. So:
//!
//! - **sRGB and ST 2084 PQ**, Rec.709 and BT.2020 primaries. These are the two
//!   states the display pipeline actually has, and they are exactly what
//!   [`crate::colour`] already maps onto.
//! - **`ext_linear` is absent.** scRGB linear is high dynamic range too, and a
//!   compositor compositing in 8-bit sRGB has nowhere to put its extended
//!   range, so a client taking the invitation would hand over linear light for
//!   this to read as encoded — a picture far too dark. It was briefly added on
//!   the theory that Gecko treats the list as a capability check and blacks out
//!   without it; measured over six runs each way that is not so (three black
//!   with it, one without), so the honest position stands. Nothing can reach a
//!   linear description while it is off this list — creating one is refused —
//!   but the mapping in [`Description::as_surface_colour`] handles it anyway,
//!   so that if the list ever does grow, linear light already reads as
//!   not-PQ and passthrough stays out of it.
//! - **No ICC**, no custom chromaticities, no power-law transfer function:
//!   nothing here converts a gamut per surface, so a description this could
//!   not act on is one it should not accept.
//!
//! ## What this does not fix
//!
//! Not the black window it was written for. With `gfx.color_management.hdr`
//! and its `force_enabled` companion both on, Zen comes up entirely black
//! *intermittently*, and it does so whether or not this global is on the
//! registry — eight nested runs each way, one capture 32 s after launch:
//!
//! | `wp_color_manager_v1` | black windows |
//! |-----------------------|---------------|
//! | advertised            | 1 of 8        |
//! | absent                | 3 of 8        |
//!
//! So the fault is not here and never was. It is consistent with Gecko's own
//! HDR path being unreliable, which `force_enabled` is a request to take
//! whatever the compositor says. What this module changes is that the pref no
//! longer *has* to be forced: a browser can now ask what the display is being
//! driven as and get a true answer.
//!
//! Two warnings for anyone measuring this again, both learned the hard way in
//! the session that wrote it. An intermittent fault cannot be read off one
//! run — every single-run comparison made here pointed the wrong way at least
//! once. And a harness that reuses one output path will happily hand back the
//! *previous* run's screenshot when the compositor failed to start, which is
//! how a build that panicked before opening its socket was recorded as
//! rendering perfectly six times.
//! - **Luminances and mastering display primaries are accepted and recorded,
//!   and nothing is done with them yet** — the same position, and for the same
//!   reason, that [`crate::colour`] takes on frog's `set_hdr_metadata`. They
//!   describe the display the content was graded on, which is only useful to a
//!   compositor that tone-maps between that and this one's. They are
//!   advertised rather than refused because a client that sends them is
//!   describing its content correctly and should not be killed for it.

use std::sync::{Arc, Mutex};

use smithay::output::Output;
use smithay::reexports::wayland_server::backend::GlobalId;
use smithay::reexports::wayland_server::protocol::wl_output::WlOutput;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};
use smithay::utils::IsAlive;
use wayland_protocols::wp::color_management::v1::server::wp_color_management_output_v1::{
    self, WpColorManagementOutputV1,
};
use wayland_protocols::wp::color_management::v1::server::wp_color_management_surface_feedback_v1::{
    self, WpColorManagementSurfaceFeedbackV1,
};
use wayland_protocols::wp::color_management::v1::server::wp_color_management_surface_v1::{
    self, WpColorManagementSurfaceV1,
};
use wayland_protocols::wp::color_management::v1::server::wp_color_manager_v1::{
    self, Feature, Primaries, RenderIntent, TransferFunction, WpColorManagerV1,
};
use wayland_protocols::wp::color_management::v1::server::wp_image_description_creator_icc_v1::{
    self, WpImageDescriptionCreatorIccV1,
};
use wayland_protocols::wp::color_management::v1::server::wp_image_description_creator_params_v1::{
    self, WpImageDescriptionCreatorParamsV1,
};
use wayland_protocols::wp::color_management::v1::server::wp_image_description_info_v1::WpImageDescriptionInfoV1;
use wayland_protocols::wp::color_management::v1::server::wp_image_description_v1::{
    self, WpImageDescriptionV1,
};

use crate::colour::{ColourHandler, SurfaceColour};

/// The interface version this implements.
///
/// Version 1 is the whole of the protocol's original shape. What versions 2
/// and 3 add — 64-bit identities, image description references, and Windows'
/// two fixed colour spaces — are all things this compositor would have to
/// answer with the same two descriptions it already has, so binding higher
/// would advertise reach it does not have.
const VERSION: u32 = 1;

/// The render intents accepted. Perceptual is the one the protocol requires of
/// every compositor, and the only one that means anything where no gamut is
/// converted per surface.
const INTENTS: &[RenderIntent] = &[RenderIntent::Perceptual];

/// See the module docs for why this list is the length it is.
const FEATURES: &[Feature] = &[
    Feature::Parametric,
    Feature::SetLuminances,
    Feature::SetMasteringDisplayPrimaries,
];

/// The transfer functions a client may name. `ext_linear` is absent; see the
/// module docs.
const TRANSFER_FUNCTIONS: &[TransferFunction] =
    &[TransferFunction::Srgb, TransferFunction::St2084Pq];

/// The primaries a client may name: what an SDR display is described as, and
/// what an HDR one is.
const PRIMARIES: &[Primaries] = &[Primaries::Srgb, Primaries::Bt2020];

// ---------------------------------------------------------------------------
// what one image description says
// ---------------------------------------------------------------------------

/// CIE 1931 xy chromaticities, in the units `wp_image_description_info_v1`
/// carries them: multiplied by a million, for six decimals.
///
/// A different scale from the one [`crate::colour`] uses for the same
/// coordinates — frog carries them in units of 0.00002 — which is exactly why
/// they are written out again here rather than converted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chromaticity {
    pub red: (i32, i32),
    pub green: (i32, i32),
    pub blue: (i32, i32),
    pub white: (i32, i32),
}

/// Rec.709's primaries, which are sRGB's, with the D65 white point.
const REC709_XY: Chromaticity = Chromaticity {
    red: (640_000, 330_000),
    green: (300_000, 600_000),
    blue: (150_000, 60_000),
    white: (312_700, 329_000),
};

/// BT.2020's, with the same white point.
const BT2020_XY: Chromaticity = Chromaticity {
    red: (708_000, 292_000),
    green: (170_000, 797_000),
    blue: (131_000, 46_000),
    white: (312_700, 329_000),
};

/// A complete image description, reduced to the two things this compositor can
/// act on plus everything a client is entitled to have handed back.
///
/// Immutable once created, which the protocol requires: a client may hold one
/// and attach it to any number of surfaces, and it has to mean the same thing
/// every time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Description {
    pub transfer: TransferFunction,
    pub primaries: Primaries,
    /// The primary colour volume's luminance range and reference white, as the
    /// protocol carries them: minimum in ten-thousandths of a candela, maximum
    /// and reference unscaled. `None` means the default for [`Self::transfer`].
    pub luminances: Option<(u32, u32, u32)>,
    /// What the client said about the display its content was graded on. See
    /// the module docs for why this is recorded and not otherwise read.
    pub target: Target,
}

/// The target colour volume: what a client says its content was mastered for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Target {
    pub primaries: Option<Chromaticity>,
    /// Minimum in ten-thousandths of a candela, maximum unscaled.
    pub luminance: Option<(u32, u32)>,
    pub max_cll: Option<u32>,
    pub max_fall: Option<u32>,
}

impl Description {
    /// What a display being driven in HDR is, and what one in SDR is.
    ///
    /// The same rule [`crate::colour::send_preferred_metadata`] applies, and it
    /// has to stay the same rule: a client is told what its pixels will
    /// actually meet, not what the panel could manage if the user asked.
    pub fn for_display(hdr: bool) -> Self {
        let (transfer, primaries) = match hdr {
            true => (TransferFunction::St2084Pq, Primaries::Bt2020),
            false => (TransferFunction::Srgb, Primaries::Srgb),
        };
        Self {
            transfer,
            primaries,
            luminances: None,
            target: Target::default(),
        }
    }

    /// The chromaticities behind [`Self::primaries`].
    fn chromaticity(&self) -> Chromaticity {
        match self.primaries {
            Primaries::Bt2020 => BT2020_XY,
            _ => REC709_XY,
        }
    }

    /// The luminance range to report, which is what the client set or the
    /// default that the named transfer characteristic implies.
    ///
    /// sRGB's are the ICC's published figures for the space. PQ's are the ones
    /// its own definition carries: it is an absolute encoding, so its maximum
    /// is a property of the curve rather than of any particular panel, and 203
    /// cd/m² is the reference white ITU-R BT.2408 puts HDR graphics at.
    fn luminances(&self) -> (u32, u32, u32) {
        self.luminances.unwrap_or(match self.transfer {
            TransferFunction::St2084Pq => (50, 10_000, 203),
            _ => (2_000, 80, 80),
        })
    }

    /// The target colour volume's chromaticities: what the client said it
    /// graded on, or — since nothing here tone-maps between the two — the
    /// primary colour volume's own.
    fn target_primaries(&self) -> Chromaticity {
        self.target.primaries.unwrap_or_else(|| self.chromaticity())
    }

    /// Likewise the target luminance range.
    fn target_luminance(&self) -> (u32, u32) {
        self.target.luminance.unwrap_or_else(|| {
            let (min, max, _) = self.luminances();
            (min, max)
        })
    }

    /// The number the protocol identifies this description by.
    ///
    /// Two descriptions that say the same thing must carry the same identity,
    /// and it must never be zero. Both fall out of packing the two enums that
    /// define one, since the lowest primaries entry is 1.
    fn identity(&self) -> u32 {
        ((self.primaries as u32) << 8) | (self.transfer as u32)
    }

    /// How this reads to the rest of the compositor.
    ///
    /// Straight onto frog's vocabulary rather than alongside it, so that one
    /// surface has one colour however it was described. Only the entries this
    /// module advertises can appear here.
    fn as_surface_colour(&self) -> SurfaceColour {
        use lxb_protocol::server::frog::frog_color_managed_surface as frog;
        SurfaceColour {
            transfer: match self.transfer {
                TransferFunction::St2084Pq => Some(frog::TransferFunction::St2084Pq),
                // Recorded as what it is rather than folded into sRGB. Both
                // read as "not PQ" to the passthrough decision, which is the
                // answer that matters, but a surface holding linear light is
                // not a surface holding sRGB and the state should not say so.
                TransferFunction::ExtLinear => Some(frog::TransferFunction::ScrgbLinear),
                _ => Some(frog::TransferFunction::Srgb),
            },
            primaries: match self.primaries {
                Primaries::Bt2020 => Some(frog::Primaries::Rec2020),
                _ => Some(frog::Primaries::Rec709),
            },
        }
    }
}

// ---------------------------------------------------------------------------
// per-object state
// ---------------------------------------------------------------------------

/// A `wp_image_description_v1`. `None` means the description failed, which the
/// protocol keeps as a live but useless object rather than an error.
#[derive(Debug)]
pub struct DescriptionData(Mutex<Option<Description>>);

/// A `wp_color_management_output_v1`, and the description it was last told.
#[derive(Debug)]
pub struct OutputData {
    output: Mutex<Option<Output>>,
    last: Mutex<Option<Description>>,
}

/// A `wp_color_management_surface_v1`.
#[derive(Debug)]
pub struct SurfaceData {
    surface: WlSurface,
}

/// A `wp_color_management_surface_feedback_v1`, and the description it was last
/// told to prefer.
#[derive(Debug)]
pub struct FeedbackData {
    surface: WlSurface,
    last: Mutex<Option<Description>>,
}

/// A `wp_image_description_creator_params_v1`: everything set so far.
#[derive(Debug, Default)]
pub struct ParamsData(Mutex<Params>);

#[derive(Debug, Default)]
struct Params {
    transfer: Option<TransferFunction>,
    primaries: Option<Primaries>,
    luminances: Option<(u32, u32, u32)>,
    target: Target,
    /// A creator is single-use: `create` consumes it, and the protocol says
    /// every later request on it is an error.
    used: bool,
}

// ---------------------------------------------------------------------------
// the global
// ---------------------------------------------------------------------------

/// The `wp_color_manager_v1` global, and the objects that outlive one request.
#[derive(Debug)]
pub struct ColourManagerState {
    /// Held for the global's lifetime.
    ///
    /// Created unconditionally, and if that ever becomes conditional again the
    /// answer is `Option<GlobalId>` and no call — **never** a version of 0.
    /// `wl_global_create` refuses any version below 1 and wayland-backend turns
    /// that refusal into a panic in `LxbState::new`, before a socket and before
    /// a connector, on every backend. It took the greeter down with the
    /// session, and `cedm` restarted into it until systemd gave up on the unit.
    _global: GlobalId,
    /// Live `wp_color_management_output_v1` objects, so a display that changes
    /// what it is being driven as can say so.
    outputs: Vec<WpColorManagementOutputV1>,
    /// Live `wp_color_management_surface_feedback_v1` objects, likewise.
    feedbacks: Vec<WpColorManagementSurfaceFeedbackV1>,
    /// Live `wp_color_management_surface_v1` objects. Kept because the protocol
    /// allows only one per surface and makes a second one an error.
    surfaces: Vec<WpColorManagementSurfaceV1>,
    /// Information objects that have been told everything except that they are
    /// finished. See [`finish_information`], which is the whole reason this
    /// list exists.
    unfinished_information: Vec<WpImageDescriptionInfoV1>,
}

impl ColourManagerState {
    pub fn new<D>(display: &DisplayHandle) -> Self
    where
        D: GlobalDispatch<WpColorManagerV1, ()> + Dispatch<WpColorManagerV1, ()> + 'static,
    {
        Self {
            _global: display.create_global::<D, WpColorManagerV1, _>(VERSION, ()),
            outputs: Vec::new(),
            feedbacks: Vec::new(),
            surfaces: Vec::new(),
            unfinished_information: Vec::new(),
        }
    }

    /// Drop everything whose client has gone.
    ///
    /// Called before each walk rather than on destruction, because a client
    /// that disconnects without destroying its objects leaves them behind and
    /// nothing else would ever notice.
    fn prune(&mut self) {
        self.outputs.retain(Resource::is_alive);
        self.feedbacks.retain(Resource::is_alive);
        self.surfaces.retain(Resource::is_alive);
    }
}

/// Send the `done` that ends each pending run of image description
/// information, and destroys the object that carried it.
///
/// This exists because `wp_image_description_info_v1.done` is a **destructor
/// event**, and wayland-rs's libwayland backend cannot survive one being sent
/// from inside the request that created the object. `resource_dispatcher` calls
/// the handler, and *then* writes the returned object data through the child's
/// user-data pointer:
///
/// ```text
/// let ret = udata.data.clone().request(...);   // done() destroys the child
/// ...
/// (*child_udata_ptr).data = child_data;        // and this writes into freed memory
/// ```
///
/// The destructor frees that allocation with `Box::from_raw`, so the write
/// lands on memory that has gone. It is a segfault in the compositor — the
/// whole session — a few hundred milliseconds after any client asks a colour
/// description what it is. Firefox asks during start-up, so this was every
/// launch.
///
/// Sending it one turn of the event loop later costs the client nothing: it is
/// waiting on a roundtrip either way, and the events it actually reads have
/// already been queued in front of this one.
pub fn finish_information<D>(state: &mut D)
where
    D: ColourManagerHandler + 'static,
{
    let pending = std::mem::take(&mut state.colour_manager_state().unfinished_information);
    for info in pending {
        if info.is_alive() {
            info.done();
        }
    }
}

/// What the compositor has to supply for this protocol to mean anything.
pub trait ColourManagerHandler: ColourHandler {
    fn colour_manager_state(&mut self) -> &mut ColourManagerState;
    /// Whether this display is *being driven* in HDR right now.
    fn output_is_hdr(&self, output: &Output) -> bool;
    /// The [`Output`] a `wl_output` resource stands for.
    fn output_for_resource(&self, resource: &WlOutput) -> Option<Output>;
}

/// Tell every client whose display or surface changed what it is now.
///
/// Called wherever the HDR pipeline reports a commit, next to the broadcast
/// that tells the shell the same news. A client that never asks again is still
/// entitled to hear that the answer moved — that is the whole point of
/// `image_description_changed`, and a browser that bound at login on an SDR
/// display would otherwise go on believing it forever.
pub fn displays_changed<D>(state: &mut D)
where
    D: ColourManagerHandler + 'static,
{
    state.colour_manager_state().prune();

    let outputs = state.colour_manager_state().outputs.clone();
    for resource in outputs {
        let Some(data) = resource.data::<Arc<OutputData>>() else {
            continue;
        };
        let Some(output) = data.output.lock().unwrap().clone() else {
            continue;
        };
        let now = Description::for_display(state.output_is_hdr(&output));
        let mut last = data.last.lock().unwrap();
        if *last == Some(now) {
            continue;
        }
        *last = Some(now);
        resource.image_description_changed();
    }

    let feedbacks = state.colour_manager_state().feedbacks.clone();
    for resource in feedbacks {
        let Some(data) = resource.data::<Arc<FeedbackData>>() else {
            continue;
        };
        let now = preferred_for(state, &data.surface);
        let mut last = data.last.lock().unwrap();
        if *last == Some(now) {
            continue;
        }
        *last = Some(now);
        resource.preferred_changed(now.identity());
    }
}

/// What a surface should be rendering, given the display it is on.
///
/// A surface on no display at all is told sRGB. That is not a placeholder: it
/// is the only encoding this compositor shows correctly without knowing which
/// panel the pixels are bound for.
fn preferred_for<D>(state: &D, surface: &WlSurface) -> Description
where
    D: ColourManagerHandler,
{
    let hdr = state
        .output_of(surface)
        .is_some_and(|output| state.output_is_hdr(&output));
    Description::for_display(hdr)
}

/// Hand a client a `wp_image_description_v1` that is already ready.
fn ready_description<D>(
    data_init: &mut DataInit<'_, D>,
    id: New<WpImageDescriptionV1>,
    description: Description,
) where
    D: Dispatch<WpImageDescriptionV1, Arc<DescriptionData>> + 'static,
{
    let resource = data_init.init(id, Arc::new(DescriptionData(Mutex::new(Some(description)))));
    resource.ready(description.identity());
}

// ---------------------------------------------------------------------------
// wp_color_manager_v1
// ---------------------------------------------------------------------------

impl<D> GlobalDispatch<WpColorManagerV1, (), D> for ColourManagerState
where
    D: GlobalDispatch<WpColorManagerV1, ()> + Dispatch<WpColorManagerV1, ()> + 'static,
{
    fn bind(
        _state: &mut D,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<WpColorManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, D>,
    ) {
        let manager = data_init.init(resource, ());
        // A client decides what to render from these four lists, so they are
        // sent on bind and the `done` that closes them is not optional.
        for intent in INTENTS {
            manager.supported_intent(*intent);
        }
        for feature in FEATURES {
            manager.supported_feature(*feature);
        }
        for tf in TRANSFER_FUNCTIONS {
            manager.supported_tf_named(*tf);
        }
        for primaries in PRIMARIES {
            manager.supported_primaries_named(*primaries);
        }
        manager.done();
    }
}

impl<D> Dispatch<WpColorManagerV1, (), D> for ColourManagerState
where
    D: Dispatch<WpColorManagerV1, ()>
        + Dispatch<WpColorManagementOutputV1, Arc<OutputData>>
        + Dispatch<WpColorManagementSurfaceV1, Arc<SurfaceData>>
        + Dispatch<WpColorManagementSurfaceFeedbackV1, Arc<FeedbackData>>
        + Dispatch<WpImageDescriptionCreatorParamsV1, Arc<ParamsData>>
        + Dispatch<WpImageDescriptionCreatorIccV1, ()>
        + Dispatch<WpImageDescriptionV1, Arc<DescriptionData>>
        + ColourManagerHandler
        + 'static,
{
    fn request(
        state: &mut D,
        _client: &Client,
        manager: &WpColorManagerV1,
        request: wp_color_manager_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            wp_color_manager_v1::Request::GetOutput { id, output } => {
                let data = Arc::new(OutputData {
                    output: Mutex::new(state.output_for_resource(&output)),
                    last: Mutex::new(None),
                });
                let resource = data_init.init(id, data);
                state.colour_manager_state().prune();
                state.colour_manager_state().outputs.push(resource);
            }
            wp_color_manager_v1::Request::GetSurface { id, surface } => {
                state.colour_manager_state().prune();
                // One per surface, and a second is a protocol error rather
                // than a silent replacement — two objects setting a colour on
                // one surface have no defined order between them.
                let taken = state
                    .colour_manager_state()
                    .surfaces
                    .iter()
                    .any(|existing| {
                        existing
                            .data::<Arc<SurfaceData>>()
                            .is_some_and(|data| data.surface == surface)
                    });
                let resource = data_init.init(
                    id,
                    Arc::new(SurfaceData {
                        surface: surface.clone(),
                    }),
                );
                if taken {
                    manager.post_error(
                        wp_color_manager_v1::Error::SurfaceExists,
                        "this surface already has a wp_color_management_surface_v1",
                    );
                    return;
                }
                state.colour_manager_state().surfaces.push(resource);
            }
            wp_color_manager_v1::Request::GetSurfaceFeedback { id, surface } => {
                let preferred = preferred_for(state, &surface);
                let resource = data_init.init(
                    id,
                    Arc::new(FeedbackData {
                        surface,
                        last: Mutex::new(Some(preferred)),
                    }),
                );
                state.colour_manager_state().prune();
                state.colour_manager_state().feedbacks.push(resource);
            }
            wp_color_manager_v1::Request::CreateParametricCreator { obj } => {
                data_init.init(obj, Arc::new(ParamsData::default()));
            }
            // Both of the below are gated on a feature this does not advertise,
            // so reaching them means the client ignored the list it was sent.
            // The object is still initialised: an uninitialised `new_id` is a
            // hole in the client's object map, and the error below is a
            // clearer thing to find in a log than whatever it would cause.
            wp_color_manager_v1::Request::CreateIccCreator { obj } => {
                data_init.init(obj, ());
                manager.post_error(
                    wp_color_manager_v1::Error::UnsupportedFeature,
                    "ICC profiles are not supported here; icc_v2_v4 was not advertised",
                );
            }
            wp_color_manager_v1::Request::CreateWindowsScrgb { image_description } => {
                data_init.init(
                    image_description,
                    Arc::new(DescriptionData(Mutex::new(None))),
                );
                manager.post_error(
                    wp_color_manager_v1::Error::UnsupportedFeature,
                    "scRGB is not supported here; windows_scrgb was not advertised",
                );
            }
            wp_color_manager_v1::Request::Destroy => {}
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// wp_color_management_output_v1
// ---------------------------------------------------------------------------

impl<D> Dispatch<WpColorManagementOutputV1, Arc<OutputData>, D> for ColourManagerState
where
    D: Dispatch<WpColorManagementOutputV1, Arc<OutputData>>
        + Dispatch<WpImageDescriptionV1, Arc<DescriptionData>>
        + ColourManagerHandler
        + 'static,
{
    fn request(
        state: &mut D,
        _client: &Client,
        _resource: &WpColorManagementOutputV1,
        request: wp_color_management_output_v1::Request,
        data: &Arc<OutputData>,
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            wp_color_management_output_v1::Request::GetImageDescription { image_description } => {
                let hdr = data
                    .output
                    .lock()
                    .unwrap()
                    .as_ref()
                    .is_some_and(|output| state.output_is_hdr(output));
                let description = Description::for_display(hdr);
                *data.last.lock().unwrap() = Some(description);
                ready_description(data_init, image_description, description);
            }
            wp_color_management_output_v1::Request::Destroy => {}
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// wp_color_management_surface_v1
// ---------------------------------------------------------------------------

impl<D> Dispatch<WpColorManagementSurfaceV1, Arc<SurfaceData>, D> for ColourManagerState
where
    D: Dispatch<WpColorManagementSurfaceV1, Arc<SurfaceData>> + ColourManagerHandler + 'static,
{
    fn request(
        state: &mut D,
        _client: &Client,
        resource: &WpColorManagementSurfaceV1,
        request: wp_color_management_surface_v1::Request,
        data: &Arc<SurfaceData>,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            wp_color_management_surface_v1::Request::SetImageDescription {
                image_description,
                render_intent,
            } => {
                let Ok(intent) = render_intent.into_result() else {
                    resource.post_error(
                        wp_color_management_surface_v1::Error::RenderIntent,
                        "unknown render intent",
                    );
                    return;
                };
                if !INTENTS.contains(&intent) {
                    resource.post_error(
                        wp_color_management_surface_v1::Error::RenderIntent,
                        "that render intent was not advertised",
                    );
                    return;
                }
                let described = image_description
                    .data::<Arc<DescriptionData>>()
                    .and_then(|data| *data.0.lock().unwrap());
                let Some(described) = described else {
                    resource.post_error(
                        wp_color_management_surface_v1::Error::ImageDescription,
                        "that image description never became ready",
                    );
                    return;
                };
                // The protocol makes this double-buffered state, to be applied
                // on the next `wl_surface.commit`. It is applied here instead,
                // as frog's is: the only thing that reads it is the per-frame
                // passthrough decision in `crate::render`, which is remade from
                // scratch every time the display is drawn, so the difference is
                // at most one frame and never a lasting disagreement.
                crate::colour::set_surface_colour(&data.surface, described.as_surface_colour());
                state.colour_changed(&data.surface);
            }
            wp_color_management_surface_v1::Request::UnsetImageDescription => {
                crate::colour::set_surface_colour(&data.surface, SurfaceColour::default());
                state.colour_changed(&data.surface);
            }
            wp_color_management_surface_v1::Request::Destroy => {
                // A surface whose colour object goes away has said nothing
                // about its colour, which is not the same as saying sRGB — but
                // it is what the compositor has to assume, and it is what frog
                // does on the same event.
                crate::colour::set_surface_colour(&data.surface, SurfaceColour::default());
                state.colour_changed(&data.surface);
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// wp_color_management_surface_feedback_v1
// ---------------------------------------------------------------------------

impl<D> Dispatch<WpColorManagementSurfaceFeedbackV1, Arc<FeedbackData>, D> for ColourManagerState
where
    D: Dispatch<WpColorManagementSurfaceFeedbackV1, Arc<FeedbackData>>
        + Dispatch<WpImageDescriptionV1, Arc<DescriptionData>>
        + ColourManagerHandler
        + 'static,
{
    fn request(
        state: &mut D,
        _client: &Client,
        resource: &WpColorManagementSurfaceFeedbackV1,
        request: wp_color_management_surface_feedback_v1::Request,
        data: &Arc<FeedbackData>,
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            wp_color_management_surface_feedback_v1::Request::GetPreferred {
                image_description,
            }
            | wp_color_management_surface_feedback_v1::Request::GetPreferredParametric {
                image_description,
            } => {
                // Both forms answer the same, because every description this
                // compositor hands out is already parametric — there is no ICC
                // profile anywhere in it to be unable to describe.
                if !data.surface.alive() {
                    resource.post_error(
                        wp_color_management_surface_feedback_v1::Error::Inert,
                        "the surface this feedback was for is gone",
                    );
                    return;
                }
                let preferred = preferred_for(state, &data.surface);
                *data.last.lock().unwrap() = Some(preferred);
                ready_description(data_init, image_description, preferred);
            }
            wp_color_management_surface_feedback_v1::Request::Destroy => {}
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// wp_image_description_creator_params_v1
// ---------------------------------------------------------------------------

impl<D> Dispatch<WpImageDescriptionCreatorParamsV1, Arc<ParamsData>, D> for ColourManagerState
where
    D: Dispatch<WpImageDescriptionCreatorParamsV1, Arc<ParamsData>>
        + Dispatch<WpImageDescriptionV1, Arc<DescriptionData>>
        + 'static,
{
    fn request(
        _state: &mut D,
        _client: &Client,
        resource: &WpImageDescriptionCreatorParamsV1,
        request: wp_image_description_creator_params_v1::Request,
        data: &Arc<ParamsData>,
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        use wp_image_description_creator_params_v1::{Error, Request};

        let mut params = data.0.lock().unwrap();
        if params.used {
            // `create` destroys the object. Anything arriving after it is a
            // client using a handle it no longer owns.
            resource.post_error(Error::AlreadySet, "this creator has already been used");
            return;
        }

        match request {
            Request::SetTfNamed { tf } => {
                if params.transfer.is_some() {
                    resource.post_error(Error::AlreadySet, "the transfer function is already set");
                    return;
                }
                let Ok(tf) = tf.into_result() else {
                    resource.post_error(Error::InvalidTf, "unknown transfer function");
                    return;
                };
                if !TRANSFER_FUNCTIONS.contains(&tf) {
                    resource.post_error(
                        Error::InvalidTf,
                        "that transfer function was not advertised",
                    );
                    return;
                }
                params.transfer = Some(tf);
            }
            Request::SetPrimariesNamed { primaries } => {
                if params.primaries.is_some() {
                    resource.post_error(Error::AlreadySet, "the primaries are already set");
                    return;
                }
                let Ok(primaries) = primaries.into_result() else {
                    resource.post_error(Error::InvalidPrimariesNamed, "unknown primaries");
                    return;
                };
                if !PRIMARIES.contains(&primaries) {
                    resource.post_error(
                        Error::InvalidPrimariesNamed,
                        "those primaries were not advertised",
                    );
                    return;
                }
                params.primaries = Some(primaries);
            }
            Request::SetLuminances {
                min_lum,
                max_lum,
                reference_lum,
            } => {
                if params.luminances.is_some() {
                    resource.post_error(Error::AlreadySet, "the luminances are already set");
                    return;
                }
                // The protocol's bound, and it is on the *minimum*: a range
                // whose top is at or below its floor describes nothing, and a
                // reference white there could never be reached. A reference
                // above the maximum is explicitly allowed and must not be
                // refused — that is what the extended target volume is for.
                // `min_lum` arrives in ten-thousandths and the other two do
                // not, so the comparison has to be made in one scale.
                let min = u64::from(min_lum);
                if u64::from(max_lum) * 10_000 <= min || u64::from(reference_lum) * 10_000 <= min {
                    resource.post_error(
                        Error::InvalidLuminance,
                        "the maximum and reference luminances must be above the minimum",
                    );
                    return;
                }
                params.luminances = Some((min_lum, max_lum, reference_lum));
            }
            Request::SetMasteringDisplayPrimaries {
                r_x,
                r_y,
                g_x,
                g_y,
                b_x,
                b_y,
                w_x,
                w_y,
            } => {
                if params.target.primaries.is_some() {
                    resource
                        .post_error(Error::AlreadySet, "the mastering primaries are already set");
                    return;
                }
                params.target.primaries = Some(Chromaticity {
                    red: (r_x, r_y),
                    green: (g_x, g_y),
                    blue: (b_x, b_y),
                    white: (w_x, w_y),
                });
            }
            // Gated by the same feature as the primaries above, so advertising
            // that one is a promise to take this one too.
            Request::SetMasteringLuminance { min_lum, max_lum } => {
                if params.target.luminance.is_some() {
                    resource
                        .post_error(Error::AlreadySet, "the mastering luminance is already set");
                    return;
                }
                params.target.luminance = Some((min_lum, max_lum));
            }
            // These two are gated by no feature at all: any client may send
            // them at any time, and refusing one would kill a client that had
            // read the advertised list correctly.
            Request::SetMaxCll { max_cll } => {
                if params.target.max_cll.is_some() {
                    resource.post_error(Error::AlreadySet, "max_cll is already set");
                    return;
                }
                params.target.max_cll = Some(max_cll);
            }
            Request::SetMaxFall { max_fall } => {
                if params.target.max_fall.is_some() {
                    resource.post_error(Error::AlreadySet, "max_fall is already set");
                    return;
                }
                params.target.max_fall = Some(max_fall);
            }
            Request::Create { image_description } => {
                // Both halves or neither: a description missing one of them
                // does not say what its pixels are.
                let (Some(transfer), Some(primaries)) = (params.transfer, params.primaries) else {
                    resource.post_error(
                        Error::IncompleteSet,
                        "an image description needs both a transfer function and primaries",
                    );
                    return;
                };
                params.used = true;
                // PQ is an absolute encoding, so the protocol fixes the top of
                // its range at ten thousand candelas above the floor whatever
                // the client asked for. Applied here rather than when the
                // luminances arrive, because the transfer function may not have
                // been named yet when they did.
                let luminances = params.luminances.map(|(min, max, reference)| {
                    match transfer == TransferFunction::St2084Pq {
                        true => (min, min / 10_000 + 10_000, reference),
                        false => (min, max, reference),
                    }
                });
                let description = Description {
                    transfer,
                    primaries,
                    luminances,
                    target: params.target,
                };
                drop(params);
                ready_description(data_init, image_description, description);
            }
            // What is left — `set_tf_power` and `set_primaries` — is gated by a
            // feature this does not advertise, so a client reaching one has
            // ignored the list it was sent on bind.
            _ => {
                resource.post_error(
                    Error::UnsupportedFeature,
                    "that image description parameter was not advertised",
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// wp_image_description_v1 and its information
// ---------------------------------------------------------------------------

impl<D> Dispatch<WpImageDescriptionV1, Arc<DescriptionData>, D> for ColourManagerState
where
    D: Dispatch<WpImageDescriptionV1, Arc<DescriptionData>>
        + Dispatch<WpImageDescriptionInfoV1, ()>
        + ColourManagerHandler
        + 'static,
{
    fn request(
        state: &mut D,
        _client: &Client,
        resource: &WpImageDescriptionV1,
        request: wp_image_description_v1::Request,
        data: &Arc<DescriptionData>,
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            wp_image_description_v1::Request::GetInformation { information } => {
                let Some(description) = *data.0.lock().unwrap() else {
                    resource.post_error(
                        wp_image_description_v1::Error::NotReady,
                        "this image description never became ready",
                    );
                    return;
                };
                let info = data_init.init(information, ());
                // The protocol makes six of these mandatory for a parametric
                // description — primaries, tf, luminances, target primaries and
                // target luminance, with primaries_named where one applies —
                // and a client that asked for information is entitled to all of
                // them. Sending a subset is what a reader of a half-filled
                // answer has to guess at, and Mesa's EGL asks for exactly this
                // set while bringing a display up.
                let volume = description.chromaticity();
                info.primaries(
                    volume.red.0,
                    volume.red.1,
                    volume.green.0,
                    volume.green.1,
                    volume.blue.0,
                    volume.blue.1,
                    volume.white.0,
                    volume.white.1,
                );
                info.primaries_named(description.primaries);
                info.tf_named(description.transfer);
                let (min, max, reference) = description.luminances();
                info.luminances(min, max, reference);
                let target = description.target_primaries();
                info.target_primaries(
                    target.red.0,
                    target.red.1,
                    target.green.0,
                    target.green.1,
                    target.blue.0,
                    target.blue.1,
                    target.white.0,
                    target.white.1,
                );
                let (target_min, target_max) = description.target_luminance();
                info.target_luminance(target_min, target_max);
                // These two are the only optional pair, and are sent only when
                // a client actually said them.
                if let Some(max_cll) = description.target.max_cll {
                    info.target_max_cll(max_cll);
                }
                if let Some(max_fall) = description.target.max_fall {
                    info.target_max_fall(max_fall);
                }
                // Everything above is safe to send from here. The `done` that
                // ends them is not, because it destroys `info` — see
                // [`finish_information`], which sends it on the next turn.
                state
                    .colour_manager_state()
                    .unfinished_information
                    .push(info);
            }
            wp_image_description_v1::Request::Destroy => {}
            _ => {}
        }
    }
}

impl<D> Dispatch<WpImageDescriptionInfoV1, (), D> for ColourManagerState
where
    D: Dispatch<WpImageDescriptionInfoV1, ()> + 'static,
{
    fn request(
        _state: &mut D,
        _client: &Client,
        _resource: &WpImageDescriptionInfoV1,
        _request: <WpImageDescriptionInfoV1 as Resource>::Request,
        _data: &(),
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        // The interface has no requests: it sends its events and is destroyed.
    }
}

impl<D> Dispatch<WpImageDescriptionCreatorIccV1, (), D> for ColourManagerState
where
    D: Dispatch<WpImageDescriptionCreatorIccV1, ()> + 'static,
{
    fn request(
        _state: &mut D,
        _client: &Client,
        _resource: &WpImageDescriptionCreatorIccV1,
        _request: wp_image_description_creator_icc_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        // Never reached: creating one of these is already a protocol error,
        // which has killed the client before it can send anything here.
    }
}

#[macro_export]
macro_rules! delegate_colour_manager {
    ($ty:ty) => {
        smithay::reexports::wayland_server::delegate_global_dispatch!($ty: [
            wayland_protocols::wp::color_management::v1::server::wp_color_manager_v1::WpColorManagerV1: ()
        ] => $crate::colour_management::ColourManagerState);
        smithay::reexports::wayland_server::delegate_dispatch!($ty: [
            wayland_protocols::wp::color_management::v1::server::wp_color_manager_v1::WpColorManagerV1: ()
        ] => $crate::colour_management::ColourManagerState);
        smithay::reexports::wayland_server::delegate_dispatch!($ty: [
            wayland_protocols::wp::color_management::v1::server::wp_color_management_output_v1::WpColorManagementOutputV1: std::sync::Arc<$crate::colour_management::OutputData>
        ] => $crate::colour_management::ColourManagerState);
        smithay::reexports::wayland_server::delegate_dispatch!($ty: [
            wayland_protocols::wp::color_management::v1::server::wp_color_management_surface_v1::WpColorManagementSurfaceV1: std::sync::Arc<$crate::colour_management::SurfaceData>
        ] => $crate::colour_management::ColourManagerState);
        smithay::reexports::wayland_server::delegate_dispatch!($ty: [
            wayland_protocols::wp::color_management::v1::server::wp_color_management_surface_feedback_v1::WpColorManagementSurfaceFeedbackV1: std::sync::Arc<$crate::colour_management::FeedbackData>
        ] => $crate::colour_management::ColourManagerState);
        smithay::reexports::wayland_server::delegate_dispatch!($ty: [
            wayland_protocols::wp::color_management::v1::server::wp_image_description_creator_params_v1::WpImageDescriptionCreatorParamsV1: std::sync::Arc<$crate::colour_management::ParamsData>
        ] => $crate::colour_management::ColourManagerState);
        smithay::reexports::wayland_server::delegate_dispatch!($ty: [
            wayland_protocols::wp::color_management::v1::server::wp_image_description_creator_icc_v1::WpImageDescriptionCreatorIccV1: ()
        ] => $crate::colour_management::ColourManagerState);
        smithay::reexports::wayland_server::delegate_dispatch!($ty: [
            wayland_protocols::wp::color_management::v1::server::wp_image_description_v1::WpImageDescriptionV1: std::sync::Arc<$crate::colour_management::DescriptionData>
        ] => $crate::colour_management::ColourManagerState);
        smithay::reexports::wayland_server::delegate_dispatch!($ty: [
            wayland_protocols::wp::color_management::v1::server::wp_image_description_info_v1::WpImageDescriptionInfoV1: ()
        ] => $crate::colour_management::ColourManagerState);
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_display_in_hdr_is_described_as_pq_and_bt2020() {
        let hdr = Description::for_display(true);
        assert_eq!(hdr.transfer, TransferFunction::St2084Pq);
        assert_eq!(hdr.primaries, Primaries::Bt2020);

        let sdr = Description::for_display(false);
        assert_eq!(sdr.transfer, TransferFunction::Srgb);
        assert_eq!(sdr.primaries, Primaries::Srgb);
    }

    #[test]
    fn only_pq_reads_as_hdr_to_the_display_pipeline() {
        // The whole point of mapping onto frog's vocabulary: the passthrough
        // decision must not be able to tell which protocol described a surface.
        assert!(Description::for_display(true).as_surface_colour().is_hdr());
        assert!(!Description::for_display(false).as_surface_colour().is_hdr());
    }

    #[test]
    fn identities_are_stable_distinct_and_never_zero() {
        let hdr = Description::for_display(true);
        let sdr = Description::for_display(false);
        assert_ne!(hdr.identity(), sdr.identity());
        assert_ne!(hdr.identity(), 0);
        assert_ne!(sdr.identity(), 0);
        // Same content, same identity — a client may compare them.
        assert_eq!(hdr.identity(), Description::for_display(true).identity());
    }

    #[test]
    fn what_a_client_said_about_mastering_never_changes_its_identity() {
        // Identity is about the colour space. Two surfaces graded on different
        // displays but encoded the same way are the same description to
        // everything this compositor does with one.
        let plain = Description::for_display(true);
        let graded = Description {
            target: Target {
                primaries: Some(BT2020_XY),
                luminance: Some((1, 1000)),
                max_cll: Some(1000),
                max_fall: Some(400),
            },
            luminances: Some((1, 1000, 203)),
            ..plain
        };
        assert_eq!(plain.identity(), graded.identity());
    }

    #[test]
    fn scrgb_linear_is_not_offered() {
        // Advertising it would invite linear light this composites as encoded.
        assert!(!TRANSFER_FUNCTIONS.contains(&TransferFunction::ExtLinear));
        assert!(TRANSFER_FUNCTIONS.contains(&TransferFunction::St2084Pq));
    }

    #[test]
    fn linear_light_is_never_mistaken_for_the_encoding_that_triggers_passthrough() {
        use lxb_protocol::server::frog::frog_color_managed_surface as frog;
        let linear = Description {
            transfer: TransferFunction::ExtLinear,
            ..Description::for_display(true)
        };
        let colour = linear.as_surface_colour();
        // Handing the client's own numbers to the cable is only ever right for
        // PQ. Linear light passed through would be read as PQ and be far too
        // dark, so this must not read as HDR to `crate::render`.
        assert!(!colour.is_hdr());
        assert_eq!(colour.transfer, Some(frog::TransferFunction::ScrgbLinear));
    }

    #[test]
    fn a_description_always_has_a_full_set_of_information_to_give() {
        // The protocol makes primaries, luminances, target primaries and target
        // luminance mandatory in a `get_information` reply. Nothing a client can
        // leave unset may make one of them unavailable, so each has a default
        // that the named transfer characteristic implies.
        for hdr in [false, true] {
            let description = Description::for_display(hdr);
            let (min, max, reference) = description.luminances();
            assert!(max > 0 && reference > 0);
            // The floor is below the ceiling in the protocol's own units.
            assert!(u64::from(max) * 10_000 > u64::from(min));
            assert_eq!(description.target_primaries(), description.chromaticity());
            assert_eq!(description.target_luminance(), (min, max));
        }
    }

    #[test]
    fn pq_and_srgb_carry_their_own_standard_luminances() {
        assert_eq!(
            Description::for_display(false).luminances(),
            (2_000, 80, 80)
        );
        // 0.005 cd/m² in ten-thousandths, PQ's ten thousand candela ceiling,
        // and BT.2408's reference white for HDR graphics.
        assert_eq!(
            Description::for_display(true).luminances(),
            (50, 10_000, 203)
        );
    }

    #[test]
    fn the_two_gamuts_are_not_the_same_chromaticities() {
        // Cheap, but this is a table of constants transcribed by hand, and
        // getting BT.2020's green wrong is invisible until a picture is wrong.
        assert_eq!(Description::for_display(true).chromaticity(), BT2020_XY);
        assert_eq!(Description::for_display(false).chromaticity(), REC709_XY);
        assert_eq!(BT2020_XY.green, (170_000, 797_000));
        assert_eq!(REC709_XY.green, (300_000, 600_000));
        // Both are D65, which is the one thing they do share.
        assert_eq!(BT2020_XY.white, REC709_XY.white);
    }
}
