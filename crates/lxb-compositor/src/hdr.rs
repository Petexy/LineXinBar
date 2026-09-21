//! High dynamic range: what the connector is told, and what the CRTC does to
//! the picture on its way there.
//!
//! Turning HDR on is two separate things that have to happen together, and a
//! session that does one without the other is worse off than one that did
//! neither.
//!
//! The first is *signalling*. A display has no way to detect that what it is
//! being sent is HDR; it has to be told, in an infoframe carried alongside the
//! video — `HDR_OUTPUT_METADATA` on the connector, saying the transfer function
//! is SMPTE ST 2084 and how bright the content was graded for — together with
//! `Colorspace`, saying the primaries are BT.2020's rather than BT.709's. Both
//! are connector properties, so only a modesetting client can set them, which
//! is why this lives in the compositor and reaches the shell as a request.
//!
//! The second is *encoding*. Everything LineXinBar composites — every client
//! surface, the shell's own layers, the cursor — is ordinary sRGB, and a
//! display told to read sRGB numbers as PQ shows a scene that is far too dark
//! and the wrong colour. Something has to convert. The CRTC already has the
//! hardware for it, in the three stages every modern display engine puts
//! between the framebuffer and the cable:
//!
//! ```text
//!   framebuffer -> DEGAMMA_LUT -> CTM -> GAMMA_LUT -> connector
//!    sRGB-coded     linear light   BT.2020   PQ-coded
//! ```
//!
//! which is exactly the conversion, one stage each: undo sRGB's transfer
//! function to get linear light, rotate BT.709's primaries onto BT.2020's, then
//! re-encode with PQ at whatever absolute luminance the user asked white to be.
//! Doing it here rather than in a shader is not only cheaper — it is the only
//! place it *can* be done without a fullscreen readback, because the frame is
//! assembled inside smithay's DRM compositor and handed straight to scanout.
//!
//! Both halves are per-connector, and smithay's atomic surface never touches
//! any of these five properties, so what is set here survives its page flips
//! and is only undone by [`Pipeline::reset`].
//!
//! # The night light
//!
//! The blue light filter lives here too, and not because it has anything to do
//! with high dynamic range. It is the *same gamma stage*: warming a picture is
//! scaling green and blue down against red, and the only place a compositor can
//! do that to everything on a screen at once is the LUT at the end of the pipe
//! above. Two pieces of code writing `GAMMA_LUT` on one CRTC would be two
//! commits fighting over one property, and whichever landed second would be the
//! whole answer — an HDR session that turned its night light on would go back to
//! being SDR-encoded, or the other way about. So there is one curve, built from
//! both, and [`Pipeline::apply`] is given both every time.
//!
//! That also makes the night light much more widely available than HDR: it
//! needs no EDID claim, no infoframe and nothing of the link, only a ramp to
//! commit. See [`Pipeline::warms`].

use std::collections::HashMap;

use smithay::output::Output;
use smithay::reexports::drm::control::atomic::AtomicModeReq;
use smithay::reexports::drm::control::{
    connector, crtc, property, AtomicCommitFlags, Device as ControlDevice, ResourceHandle,
};

/// What the user asked for, as one pipeline.
///
/// Sent and applied as a set rather than one field at a time: the LUTs and the
/// matrix are derived from all of it at once, and a half-applied change is a
/// frame of the wrong colour on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    /// Drive the display in HDR at all.
    pub enabled: bool,
    /// The luminance plain white is given, in cd/m². Every pixel LineXinBar
    /// composites is SDR, so this is the one number that decides whether the
    /// session comes out dim or blinding.
    pub sdr_brightness: u16,
    /// How far sRGB's colours are stretched into BT.2020's much wider gamut,
    /// 0 to 100. 0 converts exactly and looks like SDR did; 100 leaves the
    /// numbers alone, so sRGB's red is shown as BT.2020's red.
    pub srgb_intensity: u8,
    /// Peak luminance declared to the display, in cd/m². `None` means "use
    /// what the display says about itself".
    pub peak_brightness: Option<u16>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            // Bright enough to read as an ordinary desktop rather than as a
            // dimmed one. sRGB's own reference is 80 cd/m², which is a dark
            // grading suite; a display in a lit room is doing roughly this.
            sdr_brightness: 200,
            // Accurate, not vivid. The stretch is a preference, and a
            // preference must not be what the session comes up in.
            srgb_intensity: 0,
            peak_brightness: None,
        }
    }
}

impl Settings {
    /// The peak this pipeline should encode and declare, given what the
    /// display says about itself.
    ///
    /// Clamped below by the SDR white level: a peak under the brightness white
    /// is being asked for describes a display that cannot show its own
    /// midtones, and every value above white would be crushed into it.
    fn peak(&self, display: &Display) -> u16 {
        self.peak_brightness
            .or(display.max_luminance)
            .unwrap_or(DEFAULT_PEAK)
            .max(self.sdr_brightness)
    }
}

/// Assumed peak for a display that claims HDR without saying how bright it is.
///
/// Deliberately modest. Declaring more than the panel can do makes it tone-map
/// against a mastering level that never arrives, which shows up as a dull
/// picture; declaring less costs nothing here, because nothing LineXinBar
/// composites is brighter than the SDR white level anyway.
const DEFAULT_PEAK: u16 = 400;

/// The night light: whether one display's picture is being warmed, and how far.
///
/// Kept apart from [`Settings`] rather than folded into it, because the two
/// arrive as separate requests from separate pages and neither may overwrite
/// the other's half. They do meet — in the one gamma curve both are encoded
/// into — but that happens in [`Pipeline::apply`], which is handed both.
///
/// Nothing here says *when*. A schedule is a clock and a time zone, and the
/// shell owns both; what reaches the compositor is only whether the light
/// should be burning at this moment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NightLight {
    /// Warm the picture at all.
    pub enabled: bool,
    /// The colour temperature to warm it to, in kelvin. Lower is warmer;
    /// [`NEUTRAL_KELVIN`] is daylight and is the same picture as off.
    pub temperature: u16,
}

impl Default for NightLight {
    fn default() -> Self {
        Self {
            enabled: false,
            // Warm enough to be worth switching on and mild enough that a
            // photograph is still recognisably the colour it was — the setting
            // an evening actually wants. Only consulted when nothing has been
            // asked for, since a filter nobody enabled shows nothing anyway.
            temperature: 4000,
        }
    }
}

/// Ordinary daylight white: the temperature at which this filter is doing
/// nothing at all.
///
/// It is the identity *by construction* rather than by approximation — see
/// [`NightLight::gains`], which divides through by the white point at this
/// temperature — so a display asked for 6500 K shows exactly the picture it
/// showed with the light switched off, to the last code.
pub const NEUTRAL_KELVIN: u16 = 6500;

/// The warmest this will encode. Below it the blue channel is already at zero
/// and there is nothing left to take away.
pub const WARMEST_KELVIN: u16 = 1000;

impl NightLight {
    /// The per-channel scale the picture is multiplied by, red first.
    ///
    /// In sRGB's *coded* values rather than in linear light, which is what
    /// every gamma-ramp night light on this platform has always meant by a
    /// colour temperature — the number people have in their heads for 4000 K is
    /// the one this produces. [`Self::linear_gains`] is the same white point
    /// for the stage that works in linear light.
    ///
    /// Red is always 1: warming is taking blue and green away, never adding
    /// red, so nothing here can clip and the display never gets brighter than
    /// it was.
    ///
    /// The curve is the usual closed-form fit to the Planckian locus. A table
    /// interpolated per hundred kelvin would be a little more faithful and a
    /// great deal more to carry; what the eye is being offered is a warmth, and
    /// the fit is well inside the difference between two displays showing it.
    fn gains(self) -> [f32; 3] {
        if !self.enabled {
            return [1.0, 1.0, 1.0];
        }
        let kelvin = self
            .temperature
            .clamp(WARMEST_KELVIN, NEUTRAL_KELVIN)
            .max(WARMEST_KELVIN);
        // Divided through by the white point at daylight so that the neutral
        // temperature comes out exactly [1, 1, 1]. Without it the fit leaves
        // green and blue a percent or two short at 6500 K, and "no filter"
        // would be a slightly warm picture that nothing could take back off.
        let neutral = planckian(NEUTRAL_KELVIN);
        let wanted = planckian(kelvin);
        [
            (wanted[0] / neutral[0]).clamp(0.0, 1.0),
            (wanted[1] / neutral[1]).clamp(0.0, 1.0),
            (wanted[2] / neutral[2]).clamp(0.0, 1.0),
        ]
    }

    /// The same white point, as a scale on linear light.
    ///
    /// For the HDR curve, whose gamma stage is fed light rather than codes.
    /// sRGB's transfer function is very nearly a power law, and a power law
    /// carries a multiply through unchanged — `(c·g)^γ = c^γ·g^γ` — so decoding
    /// the coded gain is exactly the same white point expressed for the other
    /// stage, and the two paths tint a display identically.
    fn linear_gains(self) -> [f32; 3] {
        self.gains().map(srgb_to_linear)
    }

    /// Whether this actually changes the picture. A filter switched on at
    /// daylight is one nobody can see, and is not worth a commit.
    pub fn tints(self) -> bool {
        self.enabled && self.temperature < NEUTRAL_KELVIN
    }
}

/// The colour of a black body at `kelvin`, as sRGB values in 0..=1.
///
/// The closed-form approximation everything from desktop night lights to
/// photographic tools uses, good to a couple of percent across the range that
/// matters here. It is not normalised: [`NightLight::gains`] does that, and has
/// to, because the fit does not quite reach white at daylight.
fn planckian(kelvin: u16) -> [f32; 3] {
    let temperature = kelvin as f32 / 100.0;
    let red = if temperature <= 66.0 {
        255.0
    } else {
        329.698_73 * (temperature - 60.0).powf(-0.133_204_76)
    };
    let green = if temperature <= 66.0 {
        99.470_8 * temperature.ln() - 161.119_57
    } else {
        288.122_16 * (temperature - 60.0).powf(-0.075_514_85)
    };
    let blue = if temperature >= 66.0 {
        255.0
    } else if temperature <= 19.0 {
        // Below roughly 1900 K a black body has no blue left to speak of, and
        // the logarithm below would run off to negative infinity rather than
        // saying so.
        0.0
    } else {
        138.517_73 * (temperature - 10.0).ln() - 305.044_8
    };
    [
        (red / 255.0).clamp(0.0, 1.0),
        (green / 255.0).clamp(0.0, 1.0),
        (blue / 255.0).clamp(0.0, 1.0),
    ]
}

// ---------------------------------------------------------------------------
// what the session remembers, per display
// ---------------------------------------------------------------------------

/// What a shell is told about one display: whether HDR is available on it, and
/// whether it is happening.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Status {
    /// The display claims PQ *and* the driver has enough of a colour pipeline
    /// to feed it. Both, because either one alone gets nobody anywhere.
    pub supported: bool,
    /// What the connector is actually doing, as opposed to what it was asked
    /// to do. The two differ exactly when a request could not be carried out,
    /// which is the case a shell must not present as success.
    pub enabled: bool,
    /// The display's own peak, in cd/m². 0 when it does not say.
    pub max_luminance: u16,
    /// Whether [`Settings::srgb_intensity`] does anything here: there is a
    /// colour matrix on this CRTC to put it in.
    ///
    /// Reported rather than left to be discovered, because a control that
    /// silently does nothing is worse than one that is not offered.
    pub gamut: bool,
    /// Whether that conversion is the exact one — the matrix acting on linear
    /// light, because there is a degamma stage in front of it.
    ///
    /// Two answers rather than one because there are three cases and only the
    /// middle one is new. No matrix at all: nothing to offer. A matrix behind
    /// a degamma stage: the conversion, exactly. A matrix with nothing in
    /// front of it: the conversion applied to sRGB-coded values, which is
    /// right on the whole grey axis and approximate everywhere else. See
    /// [`Pipeline::converts_gamut`].
    pub gamut_exact: bool,
    /// This display's picture can be warmed: there is a gamma ramp on the pipe
    /// driving it, and this session may commit to it.
    ///
    /// A far lower bar than [`Self::supported`] and reported separately for
    /// that reason — an ordinary SDR laptop panel clears this and never comes
    /// near HDR, so a page that read one for the other would leave the night
    /// light off exactly the displays that most want it.
    pub night_light: bool,
    /// Its picture is being warmed right now. As [`Self::enabled`], this is
    /// what the compositor is doing rather than what it was asked for.
    pub warming: bool,
}

/// The session's view of HDR, kept outside the backend.
///
/// The decision arrives on the shell's Wayland connection and is carried out on
/// a DRM device, and the two do not meet: a request can land while the output
/// is between frames, on a session that is VT-switched away, or on a backend
/// with no connectors at all. So what the user asked for is recorded here, and
/// the backend picks it up at the one moment it is safe to commit — after a
/// vblank, with no page flip in flight.
///
/// Displays are remembered by connector name rather than by [`Output`], which
/// is what makes an unplugged display keep its settings: replugging one builds
/// a *new* `Output` for the same physical connector, so an HDR setting filed
/// against the old one would be lost to a cable knocked out while dusting.
#[derive(Debug, Default)]
pub struct Manager {
    displays: Vec<Entry>,
}

#[derive(Debug)]
struct Entry {
    connector: String,
    settings: Settings,
    /// The other half of the same gamma stage, asked for by its own request
    /// from its own page. See [`NightLight`].
    night: NightLight,
    status: Status,
    /// Whether the display is currently showing content that is already
    /// encoded the way the cable is, so the colour pipeline must not touch it.
    /// See [`Manager::request_passthrough`].
    passthrough: bool,
    /// Set when `settings`, `night` or `passthrough` has not reached the
    /// hardware yet.
    pending: bool,
}

impl Manager {
    /// Take note of a display the backend can drive, with the settings it
    /// should come up in and what it turns out to be capable of.
    ///
    /// Both `initial` values are only consulted the first time a connector is
    /// seen. After that the settings in force are the user's, and a display
    /// coming back comes back the way they left it.
    pub fn register(
        &mut self,
        output: &Output,
        initial: Settings,
        initial_night: NightLight,
        status: Status,
    ) {
        match self.entry(output) {
            Some(entry) => {
                entry.status = status;
                entry.pending = true;
            }
            None => self.displays.push(Entry {
                connector: output.name(),
                settings: initial,
                night: initial_night,
                status,
                passthrough: false,
                pending: true,
            }),
        }
    }

    /// Mark every display as needing its settings committed again.
    ///
    /// For coming back from a VT switch. While the session is away another
    /// client owns the DRM master, and reclaiming it means a full modeset,
    /// which puts the connector back the way the driver starts it: SDR, no
    /// metadata, an identity colour pipeline. Nothing reports that, so a
    /// session that did not re-commit would come back from a console looking
    /// washed out with the Settings column still saying HDR is on.
    pub fn reapply_all(&mut self) {
        for entry in &mut self.displays {
            entry.pending = true;
            // What the hardware is doing is no longer known; it is whatever
            // the modeset left, which is SDR with an identity ramp.
            entry.status.enabled = false;
            entry.status.warming = false;
        }
    }

    /// The same for one display, whose pipe has just been rebuilt under it.
    ///
    /// A mode set is a modeset: the driver tears the pipe down and puts it
    /// back the way it starts, which is SDR with an identity pipeline. The
    /// settings are untouched — this only says they are no longer in force.
    pub fn reapply(&mut self, output: &Output) {
        if let Some(entry) = self.entry(output) {
            entry.pending = true;
            entry.status.enabled = false;
            entry.status.warming = false;
        }
    }

    /// A display has gone away: it can do nothing until it is back, but what
    /// it was set to is still what it is set to.
    pub fn disconnected(&mut self, output: &Output) {
        if let Some(entry) = self.entry(output) {
            entry.status = Status::default();
            entry.pending = false;
        }
    }

    /// Record what the shell asked for. `true` when it is a change, and so
    /// when a redraw has to be scheduled to carry it out.
    pub fn request(&mut self, output: &Output, settings: Settings) -> bool {
        let Some(entry) = self.entry(output) else {
            // A display the backend never registered — a nested session, or a
            // connector that could not be probed. Remembering the request
            // would only make it look accepted.
            return false;
        };
        if entry.settings == settings {
            return false;
        }
        entry.settings = settings;
        entry.pending = true;
        true
    }

    /// The same for the night light, which is a request of its own.
    ///
    /// Separate from [`Self::request`] rather than another field on it,
    /// because the two come from two pages: a shell changing the white level
    /// must not have to know what the night light is set to in order to leave
    /// it alone, and the other way about.
    pub fn request_night_light(&mut self, output: &Output, night: NightLight) -> bool {
        let Some(entry) = self.entry(output) else {
            return false;
        };
        if entry.night == night {
            return false;
        }
        entry.night = night;
        entry.pending = true;
        true
    }

    /// Say whether this display is showing content that is already encoded the
    /// way the cable is.
    ///
    /// Asked of the compositor each frame rather than of the user: it is not a
    /// setting but a description of what is on screen, and it changes whenever
    /// a game goes fullscreen, the guide opens over it, or it exits. `true`
    /// when that is a change, so a steady answer costs no commits.
    ///
    /// What makes it safe to honour is that the frame is the client's buffer
    /// and nothing else — scanned out directly, with nothing composited over
    /// it. A frame the shell has drawn into is a frame with sRGB pixels in it,
    /// and those need the encode the pipeline normally does. See
    /// [`crate::render::output_shows_encoded_content`].
    pub fn request_passthrough(&mut self, output: &Output, passthrough: bool) -> bool {
        let Some(entry) = self.entry(output) else {
            return false;
        };
        if entry.passthrough == passthrough {
            return false;
        }
        entry.passthrough = passthrough;
        entry.pending = true;
        true
    }

    /// The settings a display is waiting to have applied, if it is waiting.
    ///
    /// All three together, always. They share the gamma stage, so the curve
    /// that carries one has to be built from the others as well — see
    /// [`Pipeline::apply`].
    pub fn take_pending(&mut self, output: &Output) -> Option<(Settings, NightLight, bool)> {
        let entry = self.entry(output)?;
        entry
            .pending
            .then_some((entry.settings, entry.night, entry.passthrough))
    }

    /// What one display is warmed to right now, for a session on its way out.
    ///
    /// A display nobody registered — a nested session's — is not warmed by
    /// anything here, so it answers the default, which tints nothing.
    pub fn night_light_of(&self, output: &Output) -> NightLight {
        self.displays
            .iter()
            .find(|entry| entry.connector == output.name())
            .map(|entry| entry.night)
            .unwrap_or_default()
    }

    /// Report what actually happened. `true` when that is news, and so when
    /// the shells have to be told.
    ///
    /// Clearing `pending` here rather than in [`Self::take_pending`] is what
    /// makes a commit that never happened get tried again on the next frame.
    pub fn applied(&mut self, output: &Output, applied: Applied) -> bool {
        let Some(entry) = self.entry(output) else {
            return false;
        };
        entry.pending = false;
        if entry.status.enabled == applied.enabled && entry.status.warming == applied.warming {
            return false;
        }
        entry.status.enabled = applied.enabled;
        entry.status.warming = applied.warming;
        true
    }

    /// What one display can do and is doing. Displays the backend never
    /// registered read as "no HDR here", which is the truthful answer for a
    /// nested session and for a connector that could not be probed alike.
    pub fn status(&self, output: &Output) -> Status {
        self.find(output)
            .map(|entry| entry.status)
            .unwrap_or_default()
    }

    fn find(&self, output: &Output) -> Option<&Entry> {
        let name = output.name();
        self.displays.iter().find(|entry| entry.connector == name)
    }

    fn entry(&mut self, output: &Output) -> Option<&mut Entry> {
        let name = output.name();
        self.displays
            .iter_mut()
            .find(|entry| entry.connector == name)
    }
}

// ---------------------------------------------------------------------------
// what the display says about itself
// ---------------------------------------------------------------------------

/// The HDR half of a connector's EDID.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Display {
    /// The display accepts the ST 2084 (PQ) transfer function. Without this
    /// there is no HDR to be had: the other two are a display that can be
    /// *told* about wide colour but not about high luminance.
    pub st2084: bool,
    /// The display accepts BT.2020 RGB colorimetry.
    pub bt2020: bool,
    /// Peak luminance it reports, in cd/m². `None` when it does not say, which
    /// an HDR display is entitled to do — and which is not the same as zero.
    pub max_luminance: Option<u16>,
    /// Black level it reports, in cd/m². `None` likewise.
    pub min_luminance: Option<f32>,
}

impl Display {
    /// Read a connector's EDID off the kernel and pull the HDR blocks out of
    /// it.
    ///
    /// A connector with no EDID at all is ordinary — a display that never
    /// answered its DDC lines, or a virtual output — and comes back claiming
    /// nothing, which leaves HDR unavailable rather than attempted blind.
    pub fn probe(device: &impl ControlDevice, connector: connector::Handle) -> Self {
        // The EDID lives in a blob, and the property's *value* is the id of
        // that blob — one of the two places in this module where the value
        // rather than the type is what is wanted.
        let blob = properties(device, connector)
            .get("EDID")
            .map(|property| property.value)
            .filter(|blob| *blob != 0);
        let Some(blob) = blob else {
            return Self::default();
        };
        match device.get_property_blob(blob) {
            Ok(edid) => Self::from_edid(&edid),
            Err(err) => {
                tracing::debug!(%err, "could not read the display's EDID");
                Self::default()
            }
        }
    }

    /// Read the HDR capabilities out of a raw EDID.
    ///
    /// EDID is a small, fixed, thirty-year-old binary format, and the two
    /// blocks needed here are a dozen bytes of it, so it is parsed rather than
    /// pulled in as a dependency — the same call made for `.desktop` files in
    /// the shell. Anything malformed comes back as "no HDR", which is the
    /// right failure: a display whose EDID cannot be believed is not one to
    /// start sending PQ at on a guess.
    pub fn from_edid(edid: &[u8]) -> Self {
        let mut caps = Display::default();
        // Base block, then one extension block per the count at byte 126.
        // Blocks are always 128 bytes; a truncated tail is simply not read.
        let Some(extensions) = edid.get(126).copied() else {
            return caps;
        };
        for index in 0..extensions as usize {
            let start = 128 * (index + 1);
            let Some(block) = edid.get(start..start + 128) else {
                break;
            };
            // Only CTA-861 extensions carry the data block collection; the
            // others (DisplayID, block maps) say nothing about HDR.
            if block[0] != CTA_EXTENSION_TAG {
                continue;
            }
            caps.read_cta_block(block);
        }
        caps
    }

    /// Walk one CTA-861 extension's data block collection.
    fn read_cta_block(&mut self, block: &[u8]) {
        // Byte 2 is where the detailed timing descriptors begin, and so where
        // the data blocks end. 0 means there are none at all; anything below
        // the start of the collection is nonsense.
        let end = block[2] as usize;
        if end <= CTA_COLLECTION_START || end > block.len() {
            return;
        }

        let mut at = CTA_COLLECTION_START;
        while at < end {
            let header = block[at];
            let length = (header & 0x1f) as usize;
            let tag = header >> 5;
            let Some(payload) = block.get(at + 1..at + 1 + length) else {
                return;
            };
            // Only the extended tags matter here, and the extended tag is the
            // first byte of the payload.
            if tag == CTA_TAG_EXTENDED {
                match payload.first().copied() {
                    Some(CTA_EXTENDED_COLORIMETRY) => self.read_colorimetry(&payload[1..]),
                    Some(CTA_EXTENDED_HDR_STATIC) => self.read_hdr_static(&payload[1..]),
                    _ => {}
                }
            }
            at += 1 + length;
        }
    }

    /// The Colorimetry Data Block: which wide-gamut encodings the display
    /// accepts. Bit 7 of its first byte is BT.2020 RGB, which is the one the
    /// `Colorspace` property will be set to.
    fn read_colorimetry(&mut self, payload: &[u8]) {
        if let Some(byte) = payload.first() {
            self.bt2020 |= byte & 0x80 != 0;
        }
    }

    /// The HDR Static Metadata Data Block: the transfer functions the display
    /// accepts, and the luminances it would like content graded for.
    ///
    /// The three luminance bytes are optional and are frequently absent — the
    /// block is allowed to stop after the first two — so each is read only if
    /// it is there rather than defaulted to zero, which would read as a
    /// display that cannot show anything.
    fn read_hdr_static(&mut self, payload: &[u8]) {
        let Some(transfer) = payload.first() else {
            return;
        };
        self.st2084 |= transfer & 0x04 != 0;

        // Both curves are the CTA's own, and both are exponential: the code is
        // a step on a scale that starts at 50 cd/m² and doubles every 32.
        if let Some(code) = payload.get(2).copied() {
            let max = 50.0 * 2f32.powf(code as f32 / 32.0);
            self.max_luminance = Some(max.round().clamp(0.0, u16::MAX as f32) as u16);
            // The black level is a fraction of the peak, squared, which is how
            // the CTA gets four useful digits out of one byte.
            if let Some(code) = payload.get(4).copied() {
                let fraction = code as f32 / 255.0;
                self.min_luminance = Some(max * fraction * fraction / 100.0);
            }
        }
    }
}

const CTA_EXTENSION_TAG: u8 = 0x02;
/// Byte 4: the collection starts after the tag, revision and DTD offset.
const CTA_COLLECTION_START: usize = 4;
const CTA_TAG_EXTENDED: u8 = 7;
const CTA_EXTENDED_COLORIMETRY: u8 = 5;
const CTA_EXTENDED_HDR_STATIC: u8 = 6;

// ---------------------------------------------------------------------------
// what the hardware can do
// ---------------------------------------------------------------------------

/// The five KMS properties HDR is made of, resolved once when the connector is
/// discovered.
///
/// Every one of them is optional, and the combinations are not academic: a
/// laptop panel may have the colour pipeline and no HDR metadata property, an
/// external display may have the metadata and a driver with no degamma stage.
/// What is missing decides how much of the conversion can be done, which
/// [`Pipeline::describe`] spells out into the log rather than leaving as a
/// picture that is subtly wrong.
#[derive(Debug, Default)]
pub struct Pipeline {
    /// `Colorspace` on the connector, with the raw values of the two enum
    /// entries used: BT.2020 RGB, and the default the display came up in.
    colorspace: Option<(property::Handle, u64, u64)>,
    /// `HDR_OUTPUT_METADATA` on the connector.
    metadata: Option<property::Handle>,
    /// `DEGAMMA_LUT` on the CRTC, and the number of entries it takes.
    degamma: Option<(property::Handle, usize)>,
    /// `CTM` on the CRTC.
    ctm: Option<property::Handle>,
    /// `GAMMA_LUT` on the CRTC, and its size.
    gamma: Option<(property::Handle, usize)>,
    /// Whether this device took the atomic client capability. Everything here
    /// is committed atomically, so a driver still on the legacy interface can
    /// be told none of it — and is better off being told so up front than
    /// finding out from a rejected commit once the user has asked.
    atomic: bool,
    /// Blobs this pipeline created and the kernel is still holding, freed one
    /// generation later. Destroying a blob the moment after it is committed is
    /// legal — the atomic state holds its own reference — but keeping it until
    /// it has been replaced costs four handles and cannot race.
    blobs: Vec<u64>,
    /// Whether this connector has ever been driven into HDR.
    ///
    /// Sticky, and deliberately: from the first time the signal was described
    /// to the display, it is described every time. Clearing it on the way back
    /// to SDR would mean the *next* change dropped the infoframe that had just
    /// said so, which is the silence this exists to avoid. See
    /// [`Self::stage_reset`].
    signalled: bool,
}

/// What one call to [`Pipeline::apply`] did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Applied {
    /// Whether the display ended up in HDR.
    pub enabled: bool,
    /// Whether it ended up with a warmed picture.
    pub warming: bool,
    /// Whether the driver took the request.
    ///
    /// Separate from `enabled` because taking a display *out* of HDR is a
    /// commit that succeeded and left it in SDR, and the caller has to tell
    /// those apart: what changed the CRTC is the commit, not the outcome.
    pub committed: bool,
}

impl Pipeline {
    /// Resolve the properties for one connector and the CRTC driving it.
    pub fn probe(
        device: &impl ControlDevice,
        connector: connector::Handle,
        crtc: crtc::Handle,
        atomic: bool,
    ) -> Self {
        let connector_props = properties(device, connector);
        let crtc_props = properties(device, crtc);

        // Colorspace is the one place a property's *type* is the right thing
        // to read: which entries the enumeration has, and what each is called.
        // The names are matched rather than the numbers, because the numbers
        // are positions in a kernel list that has grown twice.
        let colorspace = connector_props.get("Colorspace").and_then(|property| {
            let property::ValueType::Enum(values) = property.info.value_type() else {
                return None;
            };
            let (_, entries) = values.values();
            let named = |name: &str| {
                entries
                    .iter()
                    .find(|entry| entry.name().to_bytes() == name.as_bytes())
                    .map(|entry| entry.value())
            };
            Some((
                property.info.handle(),
                named("BT2020_RGB")?,
                named("Default")?,
            ))
        });

        let pipeline = Self {
            colorspace,
            metadata: connector_props
                .get("HDR_OUTPUT_METADATA")
                .map(|property| property.info.handle()),
            degamma: lut(&crtc_props, "DEGAMMA_LUT", "DEGAMMA_LUT_SIZE"),
            ctm: crtc_props.get("CTM").map(|property| property.info.handle()),
            gamma: lut(&crtc_props, "GAMMA_LUT", "GAMMA_LUT_SIZE"),
            atomic,
            blobs: Vec::new(),
            // A connector this has never driven is in whatever state the
            // driver brought it up in, which is SDR.
            signalled: false,
        };

        // The sizes go in the log at startup rather than only appearing in a
        // curve nobody can see. They are the numbers everything else here is
        // allocated from, and the one time they were read wrong the session
        // did not survive it.
        tracing::debug!(
            atomic,
            colorspace = pipeline.colorspace.is_some(),
            metadata = pipeline.metadata.is_some(),
            ctm = pipeline.ctm.is_some(),
            degamma_entries = pipeline.degamma.map(|(_, size)| size),
            gamma_entries = pipeline.gamma.map(|(_, size)| size),
            "colour pipeline"
        );
        pipeline
    }

    /// Whether this connector can be driven in HDR at all.
    ///
    /// The metadata property is the hard requirement: without it the display
    /// is never told the signal changed, and a PQ-encoded picture sent to a
    /// display still expecting sRGB is far worse than no HDR. A colour
    /// pipeline that can re-encode is the other half — [`Self::gamma`] alone
    /// is enough for that, since the sRGB decode can be folded into it.
    pub fn supported(&self) -> bool {
        self.atomic && self.metadata.is_some() && self.gamma.is_some()
    }

    /// Whether the gamut can be converted here at all, which is what
    /// [`Settings::srgb_intensity`] asks for: a matrix on the CRTC to put the
    /// conversion in.
    ///
    /// Not the same question as whether it can be converted *exactly* — see
    /// [`Self::converts_gamut_exactly`], and the note on the matrix in
    /// [`Self::apply`] for what the difference is worth.
    pub fn converts_gamut(&self) -> bool {
        self.ctm.is_some()
    }

    /// Whether that conversion acts on linear light, which is the only way it
    /// is the conversion it claims to be.
    ///
    /// A matrix is a gamut rotation when it acts on linear light; in front of
    /// a degamma stage it is that, and behind nothing it is an approximation
    /// of it. Both are offered, and the page says which one the user is
    /// getting — see [`Self::apply`].
    pub fn converts_gamut_exactly(&self) -> bool {
        self.ctm.is_some() && self.degamma.is_some()
    }

    /// Whether this connector's picture can be warmed, which is what the night
    /// light asks for: a gamma ramp, and an atomic commit to put a curve in it.
    ///
    /// Nothing else. No EDID claim, no infoframe, nothing of what the link can
    /// carry — a tint is three numbers multiplied into a lookup table that is
    /// already there. It is deliberately a much shorter list than
    /// [`Self::supported`], and the reason the night light is offered on
    /// displays HDR is not.
    pub fn warms(&self) -> bool {
        self.atomic && self.gamma.is_some()
    }

    /// What this pipeline will and will not be able to do, for the log. Said
    /// when the display is driven into HDR, because every one of these is a
    /// visible difference that the user would otherwise have to diagnose from
    /// the picture.
    pub fn describe(&self) -> &'static str {
        match (self.degamma.is_some(), self.ctm.is_some()) {
            (true, true) => "full pipeline",
            // Nothing to put the conversion in. The picture keeps its
            // brightness and its primaries are sent as BT.2020's, which is the
            // most saturated end of the setting whatever it says.
            (_, false) => "no CTM: sRGB's primaries are sent as BT.2020's",
            // A matrix with no linear stage in front of it. The rotation is
            // applied to sRGB-coded values, which leaves the grey axis exact
            // and every saturated colour close rather than right.
            (false, true) => "no degamma LUT: the gamut is converted approximately",
        }
    }

    /// Put `settings` and `night` into force on this connector, and say what
    /// the display ended up doing.
    ///
    /// All five properties go in **one** atomic request, and that request is
    /// offered to the driver as a test before it is committed for real.
    ///
    /// One request because each of these can force a modeset: setting them one
    /// ioctl at a time is up to five modesets in a row, which on a DisplayPort
    /// link means five rounds of retraining and five black screens for what the
    /// user asked to be a single change. Atomic is what the interface is for.
    ///
    /// The test pass is the safety net. A LUT this hardware will not take,
    /// BT.2020 on a link that cannot carry it, a metadata blob a driver
    /// dislikes — every one of those comes back as an error from a commit that
    /// changed nothing, instead of being attempted on a live display and
    /// leaving the session to find out.
    ///
    /// Both settings arrive together because they meet in the gamma stage: the
    /// night light's white point is multiplied into whichever curve that stage
    /// is carrying, so turning a filter on cannot undo the tone mapping and
    /// turning HDR on cannot undo the filter.
    // Seven of these are the question and the eighth is the answer's context;
    // splitting them into a struct would name a thing that has no life outside
    // this call. The three that vary together — settings, night light and
    // passthrough — are exactly the three the one gamma curve is built from,
    // which is the invariant this module opens by explaining.
    #[allow(clippy::too_many_arguments)]
    pub fn apply(
        &mut self,
        device: &impl ControlDevice,
        connector: connector::Handle,
        crtc: crtc::Handle,
        display: &Display,
        settings: &Settings,
        night: &NightLight,
        passthrough: bool,
    ) -> Applied {
        let on = settings.enabled && self.supported() && display.st2084;
        // Only ever inside HDR: passthrough means "the content is already
        // encoded the way the cable is", and outside HDR the cable is sRGB,
        // which is what everything composited here already is. There would be
        // nothing to pass through.
        let passthrough = passthrough && on;
        // A warm ramp scales linear light. Passthrough hands the client's own
        // PQ-coded values to the cable untouched, and scaling those is not the
        // same operation at all — it would darken the picture unevenly rather
        // than warm it. So the night light stands down for as long as a game
        // is driving the display's colour, and `Applied` reports that honestly
        // rather than claiming a warmth nobody applied.
        let warm = night.tints() && self.warms() && !passthrough;
        let mut request = AtomicModeReq::new();
        let mut fresh = Vec::new();

        if on {
            let peak = settings.peak(display);
            // In linear light, because this curve's input is: the degamma
            // stage has already undone sRGB, or the decode below is about to.
            let gains = if warm {
                night.linear_gains()
            } else {
                [1.0, 1.0, 1.0]
            };

            if let Some((handle, size)) = self.degamma {
                let curve: Vec<ColorLut> = (0..size)
                    .map(|index| {
                        let coded = index as f32 / (size - 1) as f32;
                        // Passthrough: the identity. The client's values are
                        // already PQ-coded BT.2020, so undoing an sRGB
                        // transfer function they were never encoded with is
                        // the exact mistake this mode exists to stop.
                        ColorLut::grey(if passthrough {
                            coded
                        } else {
                            srgb_to_linear(coded)
                        })
                    })
                    .collect();
                self.stage_blob(device, &mut request, crtc, handle, cast(&curve), &mut fresh);
            }

            // The gamut matrix. Behind a degamma stage it is the conversion
            // exactly; with nothing in front of it the same matrix acts on
            // sRGB-coded values, which is not the same operation — and is
            // still far closer to it than leaving the matrix out.
            //
            // It used to be dropped in that case, on the grounds that an
            // approximation is not the thing it approximates. What that cost
            // was never neutral: no matrix is the *identity*, and the identity
            // is exactly the 100 end of this setting — sRGB's primaries sent
            // as BT.2020's. A display engine with no degamma LUT was pinned at
            // the most saturated picture there is, and 0, the default, could
            // not be reached at all.
            //
            // Every row of this matrix sums to 1, so white, black and the
            // whole grey axis come through untouched whatever the values are
            // encoded in; the cost of the missing linear stage falls entirely
            // on saturated colour. Measured against the exact conversion over
            // a 17³ sRGB grid, in ΔE*ab: mean 9.0, 95th percentile 26.6
            // applying it here, against 26.8 and 63.4 for not applying it at
            // all. Ordinary picture content is far nearer than the mean —
            // skin 0.9, sky 2.2, the shell's own violet 2.1.
            //
            // So it is applied either way, and `Status::gamut_exact` carries
            // which of the two this is so the page can say so. This is not a
            // rare corner: amdgpu withholds `DEGAMMA_LUT` on DCN 4.01 — every
            // RDNA 4 card — because a pre-blending degamma LUT would not apply
            // to the cursor.
            if let Some(handle) = self.ctm {
                if passthrough {
                    // The client sent BT.2020 already; rotating it again would
                    // move it somewhere nothing asked for. Identity, and set
                    // explicitly rather than left alone: what this request
                    // installs has to be the whole of the CRTC's colour state,
                    // and a matrix inherited from whatever was there before
                    // would be a setting nobody chose and nothing reports.
                    request.add_property(crtc, handle, property::Value::Blob(0));
                } else {
                    let matrix = ColorCtm::gamut(settings.srgb_intensity);
                    self.stage_blob(
                        device,
                        &mut request,
                        crtc,
                        handle,
                        cast(&[matrix]),
                        &mut fresh,
                    );
                }
            }

            if let Some((handle, size)) = self.gamma {
                // Without a degamma stage this curve carries the sRGB decode
                // too, so the tone mapping is exactly right even where the
                // gamut conversion above it could only be approximate.
                let decode = self.degamma.is_none();
                let curve: Vec<ColorLut> = (0..size)
                    .map(|index| {
                        let coded = index as f32 / (size - 1) as f32;
                        // Passthrough: the identity again, and the stage that
                        // matters most. Re-encoding a value that is already PQ
                        // is what would blow the picture out.
                        if passthrough {
                            return ColorLut::grey(coded);
                        }
                        let relative = if decode { srgb_to_linear(coded) } else { coded };
                        // No clamp against the peak: `Settings::peak` will not
                        // return one below the white level, and white is the
                        // brightest thing there is to encode. The night light
                        // only ever scales down, so it cannot reach one either.
                        let nits =
                            |gain: f32| pq_encode(relative * gain * settings.sdr_brightness as f32);
                        ColorLut::rgb(nits(gains[0]), nits(gains[1]), nits(gains[2]))
                    })
                    .collect();
                self.stage_blob(device, &mut request, crtc, handle, cast(&curve), &mut fresh);
            }

            if let Some((handle, bt2020, _)) = self.colorspace {
                // The value carried is the raw one the enumeration gave for
                // that name; how it is wrapped here only decides which
                // conversion `RawValue` goes through, and every one of them
                // passes an integer straight along.
                request.add_property(connector, handle, property::Value::UnsignedRange(bt2020));
            }
            if let Some(handle) = self.metadata {
                let metadata = HdrOutputMetadata::st2084(settings.sdr_brightness, peak, display);
                self.stage_blob(
                    device,
                    &mut request,
                    connector,
                    handle,
                    cast(&[metadata]),
                    &mut fresh,
                );
            }
        } else {
            // Out of HDR, but not necessarily back to an identity ramp: a
            // display can be warm and SDR at the same time, and much the most
            // usual night light is exactly that one.
            let warmth = warm.then(|| night.gains());
            self.stage_reset(device, &mut request, connector, crtc, warmth, &mut fresh);
        }

        let committed = commit(device, request);
        if committed {
            // Only now: the kernel took its own reference to each of these
            // when the commit landed, so the previous generation is safe to
            // let go of and this one is what has to be kept.
            self.free_blobs(device);
            self.blobs = fresh;
            self.signalled |= on;
        } else {
            // Nothing changed on screen, so nothing is holding these.
            for blob in fresh {
                let _ = device.destroy_property_blob(blob);
            }
            if !on {
                tracing::warn!("could not take this display back out of HDR");
            }
        }
        Applied {
            enabled: on && committed,
            warming: warm && committed,
            committed,
        }
    }

    /// Take the connector back out of HDR: identity colour pipeline, the
    /// colorimetry the display came up in, and the signal handed back to SDR.
    ///
    /// `night` is what the display is warmed to, and it is *kept*. HDR is the
    /// thing that has to be undone — a display left in BT.2020/PQ shows
    /// whatever comes next through a transfer function it knows nothing about,
    /// and what comes next may be a console the user needs to read. A warm
    /// ramp is not like that. It is a slightly amber picture, which is what
    /// the user asked every display on this machine for at this hour, and it
    /// is what the next compositor is about to ask for again.
    ///
    /// Handing it back cold was a visible seam once the black screen between
    /// two sessions stopped being black: the last frame of the outgoing
    /// session would go cool, hold for the second it takes the next compositor
    /// to take the displays, and warm again on its first frame. Three states
    /// where the user asked for one.
    pub fn reset(
        &mut self,
        device: &impl ControlDevice,
        connector: connector::Handle,
        crtc: crtc::Handle,
        night: &NightLight,
    ) {
        let mut request = AtomicModeReq::new();
        let mut fresh = Vec::new();
        let warmth = (night.tints() && self.warms()).then(|| night.gains());
        self.stage_reset(device, &mut request, connector, crtc, warmth, &mut fresh);
        if commit(device, request) {
            self.free_blobs(device);
            self.blobs = fresh;
        } else {
            for blob in fresh {
                let _ = device.destroy_property_blob(blob);
            }
        }
    }

    /// The request that undoes everything [`Self::apply`] sets.
    ///
    /// A blob id of 0 is how KMS spells "no blob", and for the three colour
    /// stages that is the identity — the hardware is bypassed rather than
    /// loaded with a straight line, which is both faster and exactly right.
    ///
    /// The metadata is the exception, and the reason this needs a device at
    /// all. Dropping the property to 0 stops the infoframe outright, and a
    /// sink that simply stops hearing about HDR is not thereby told the signal
    /// went back to SDR: CTA-861.3 has the source say so, by sending the same
    /// infoframe with the traditional-gamma EOTF. An HDMI display left to work
    /// it out for itself may keep decoding PQ, and what it makes of an SDR
    /// picture that way can be a black screen until something puts it back.
    /// So the way out of HDR is one more infoframe, not the absence of one.
    ///
    /// Only where HDR has actually been signalled, though. A connector this
    /// has never driven into it is left at 0, so a session whose displays are
    /// all in SDR does not begin with a modeset apiece to announce it.
    ///
    /// `warmth` is the one thing that survives the reset: an SDR display with
    /// the night light on is a display whose gamma stage is doing something,
    /// and blanking that here is what would make turning HDR off take the
    /// filter with it. Every other stage still goes back to identity — a
    /// tinted picture is a scaled ramp and nothing else.
    fn stage_reset(
        &self,
        device: &impl ControlDevice,
        request: &mut AtomicModeReq,
        connector: connector::Handle,
        crtc: crtc::Handle,
        warmth: Option<[f32; 3]>,
        fresh: &mut Vec<u64>,
    ) {
        if let Some(handle) = self.metadata {
            if self.signalled {
                self.stage_blob(
                    device,
                    request,
                    connector,
                    handle,
                    cast(&[HdrOutputMetadata::sdr()]),
                    fresh,
                );
            } else {
                request.add_property(connector, handle, property::Value::Blob(0));
            }
        }
        if let Some((handle, _, default)) = self.colorspace {
            request.add_property(connector, handle, property::Value::UnsignedRange(default));
        }
        // The gamma stage first, because it is the one that may not be blank.
        if let Some((handle, size)) = self.gamma {
            match warmth {
                // Decoded, scaled, encoded again. The gains are sRGB's own
                // coded values, so this is very nearly `coded * gain` — but
                // only very nearly, and doing it in light is what makes the
                // SDR filter and the HDR one the same white point rather than
                // two that agree in the midtones and part in the shadows.
                Some(gains) => {
                    let curve: Vec<ColorLut> = (0..size)
                        .map(|index| {
                            let light = srgb_to_linear(index as f32 / (size - 1) as f32);
                            let coded = |gain: f32| linear_to_srgb(light * srgb_to_linear(gain));
                            ColorLut::rgb(coded(gains[0]), coded(gains[1]), coded(gains[2]))
                        })
                        .collect();
                    self.stage_blob(device, request, crtc, handle, cast(&curve), fresh);
                }
                None => request.add_property(crtc, handle, property::Value::Blob(0)),
            }
        }
        for handle in [self.ctm, self.degamma.map(|(handle, _)| handle)]
            .into_iter()
            .flatten()
        {
            request.add_property(crtc, handle, property::Value::Blob(0));
        }
    }

    /// Create a blob, put it in the request, and remember it for freeing.
    fn stage_blob<H: ResourceHandle>(
        &self,
        device: &impl ControlDevice,
        request: &mut AtomicModeReq,
        object: H,
        handle: property::Handle,
        data: &[u8],
        fresh: &mut Vec<u64>,
    ) {
        match create_blob(device, data) {
            Ok(blob) => {
                fresh.push(blob);
                request.add_property(object, handle, property::Value::Blob(blob));
            }
            // Leaving this stage out of the request rather than abandoning the
            // whole change: the test pass below decides whether what is left
            // is something the driver will take.
            Err(err) => tracing::warn!(%err, "could not create a colour pipeline blob"),
        }
    }

    fn free_blobs(&mut self, device: &impl ControlDevice) {
        for blob in self.blobs.drain(..) {
            if let Err(err) = device.destroy_property_blob(blob) {
                tracing::debug!(%err, blob, "could not free a property blob");
            }
        }
    }
}

/// Offer a request to the driver, then commit it if the driver says it is
/// valid. `false` when nothing was changed.
///
/// `ALLOW_MODESET` because changing the colorimetry a link carries is one, and
/// blocking because the caller has arranged to be between frames — this runs
/// with no page flip in flight on the CRTC, which is why it cannot be waiting
/// on one.
fn commit(device: &impl ControlDevice, request: AtomicModeReq) -> bool {
    if let Err(err) = device.atomic_commit(
        AtomicCommitFlags::ALLOW_MODESET | AtomicCommitFlags::TEST_ONLY,
        request.clone(),
    ) {
        tracing::warn!(%err, "the driver rejected this colour pipeline; the display is untouched");
        return false;
    }
    if let Err(err) = device.atomic_commit(AtomicCommitFlags::ALLOW_MODESET, request) {
        tracing::warn!(%err, "could not commit the colour pipeline");
        return false;
    }
    true
}

/// A LUT property and the number of entries the hardware wants in it.
///
/// The size is the **value** of the `_SIZE` property sitting beside it, not
/// anything about that property's type. The kernel declares both size
/// properties as immutable ranges over `0..=u32::MAX` and then sets each one to
/// the real figure — 4096 on amdgpu — so reading the range's upper bound gives
/// four billion, and a curve built to that length is 34 GB of allocation
/// performed inside the render loop. That is not a theoretical hazard: it is
/// what the first version of this did, and it took a machine down hard enough
/// to need the power switch. Hence [`lut_entries`], which is tested.
fn lut(
    props: &HashMap<String, Property>,
    name: &str,
    size: &str,
) -> Option<(property::Handle, usize)> {
    let handle = props.get(name)?.info.handle();
    let entries = lut_entries(props.get(size)?.value)?;
    Some((handle, entries))
}

/// Turn a reported LUT size into one this is willing to allocate.
///
/// Rejected rather than clamped when it is out of range. A driver asking for
/// something absurd is a driver this code does not understand, and handing it a
/// curve of a length it did not ask for would be a guess written into the
/// picture; going without the stage is the honest answer, and
/// [`Pipeline::describe`] already has words for a missing one.
fn lut_entries(size: u64) -> Option<usize> {
    // Two is the least that can describe a curve, and the arithmetic that
    // fills one divides by one less than this. The ceiling is generous — the
    // largest LUT any current display engine asks for is a few thousand — and
    // exists only so that no reading of this number, however wrong, can put
    // the session into swap.
    const MAX_ENTRIES: u64 = 64 * 1024;
    (2..=MAX_ENTRIES).contains(&size).then_some(size as usize)
}

/// One property of one object: what it is, and what it is currently set to.
///
/// The two are kept together because they are not interchangeable and reading
/// one for the other is a mistake that compiles. `GAMMA_LUT_SIZE` is the
/// example that matters: it is declared as a range over `0..=u32::MAX`, and its
/// *value* is the number of entries the hardware wants. Taking the range's
/// upper bound for the size asks for a four-billion-entry lookup table.
struct Property {
    info: property::Info,
    value: u64,
}

/// Every property of one object, by name, with its current value.
fn properties<T: ResourceHandle>(
    device: &impl ControlDevice,
    handle: T,
) -> HashMap<String, Property> {
    let Ok(set) = device.get_properties(handle) else {
        return HashMap::new();
    };
    let mut out = HashMap::new();
    for (handle, value) in set.iter() {
        let Ok(info) = device.get_property(*handle) else {
            continue;
        };
        let Ok(name) = info.name().to_str().map(str::to_owned) else {
            continue;
        };
        out.insert(
            name,
            Property {
                info,
                value: *value,
            },
        );
    }
    out
}

/// Create a property blob from raw bytes.
///
/// `drm`'s own helper sizes the blob from the type it is handed, which cannot
/// express a LUT whose length the driver chose at runtime, so the ioctl is
/// called directly. The kernel copies the data, so the buffer is ours again as
/// soon as this returns.
fn create_blob(device: &impl ControlDevice, data: &[u8]) -> std::io::Result<u64> {
    let mut data = data.to_vec();
    let blob = drm_ffi::mode::create_property_blob(device.as_fd(), &mut data)?;
    Ok(blob.blob_id.into())
}

fn cast<T: Copy>(values: &[T]) -> &[u8] {
    // SAFETY: every type this is used with is `#[repr(C)]` and made entirely
    // of integers, so it has no invalid bit patterns; where alignment would
    // leave a hole, the structure names it as a field and fills it, so there
    // is no padding to leak either.
    unsafe {
        std::slice::from_raw_parts(values.as_ptr().cast::<u8>(), std::mem::size_of_val(values))
    }
}

// ---------------------------------------------------------------------------
// the KMS structures, and the arithmetic that fills them
// ---------------------------------------------------------------------------

/// One entry of `struct drm_color_lut`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct ColorLut {
    red: u16,
    green: u16,
    blue: u16,
    reserved: u16,
}

impl ColorLut {
    /// The same value in all three channels: a tone curve, where the colour is
    /// the matrix's job.
    fn grey(value: f32) -> Self {
        Self::rgb(value, value, value)
    }

    /// Three channels that differ, which is what the night light needs and the
    /// only reason this curve is ever anything but grey. Warming a picture is
    /// pulling green and blue down against red, and a ramp is the one stage
    /// that can do it without a matrix in front of it.
    fn rgb(red: f32, green: f32, blue: f32) -> Self {
        let level = |value: f32| (value.clamp(0.0, 1.0) * u16::MAX as f32).round() as u16;
        Self {
            red: level(red),
            green: level(green),
            blue: level(blue),
            reserved: 0,
        }
    }
}

/// `struct drm_color_ctm`: a 3×3 matrix in S31.32 *sign-magnitude* fixed
/// point, which is not two's complement and is worth saying out loud.
#[repr(C)]
#[derive(Clone, Copy)]
struct ColorCtm {
    matrix: [u64; 9],
}

impl ColorCtm {
    /// The matrix that carries linear BT.709 light into BT.2020's primaries,
    /// blended `intensity` of the way back towards doing nothing at all.
    ///
    /// At 0 this is the exact conversion: sRGB content shown on a BT.2020
    /// signal looks the way it did in SDR, because its colours are placed
    /// where they actually belong inside the wider gamut. At 100 it is the
    /// identity, so each primary is sent straight through and the display
    /// renders sRGB's red with BT.2020's much more saturated one — the "vivid"
    /// mode a television ships in, and the reason this is a preference rather
    /// than a constant.
    ///
    /// The blend is between two matrices whose rows each sum to 1 and whose
    /// entries are all positive, so every matrix in between has both
    /// properties too: white stays white, and nothing can clip.
    fn gamut(intensity: u8) -> Self {
        let towards_identity = intensity.min(100) as f32 / 100.0;
        let mut matrix = [0u64; 9];
        for row in 0..3 {
            for column in 0..3 {
                let exact = BT709_TO_BT2020[row][column];
                let identity = if row == column { 1.0 } else { 0.0 };
                let value = exact + (identity - exact) * towards_identity;
                // Non-negative throughout, so the sign bit is never set and
                // the magnitude is the whole of it.
                matrix[row * 3 + column] = (value.max(0.0) as f64 * FIXED_POINT).round() as u64;
            }
        }
        Self { matrix }
    }
}

/// 2³², the scale of the CTM's 32-bit fraction.
const FIXED_POINT: f64 = 4_294_967_296.0;

/// Linear BT.709 RGB into linear BT.2020 RGB, both normalised to D65 white.
const BT709_TO_BT2020: [[f32; 3]; 3] = [
    [0.627_403_9, 0.329_283_04, 0.043_313_06],
    [0.069_097_29, 0.919_540_4, 0.011_362_32],
    [0.016_391_44, 0.088_013_31, 0.895_595_25],
];

/// `struct hdr_metadata_infoframe`: the CTA-861 static metadata the display
/// reads to know what it is being sent.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct HdrMetadataInfoframe {
    eotf: u8,
    metadata_type: u8,
    /// Red, green and blue chromaticity, in units of 0.00002.
    display_primaries: [[u16; 2]; 3],
    white_point: [u16; 2],
    /// The mastering display's peak, in whole cd/m².
    max_display_mastering_luminance: u16,
    /// Its black level, in units of 0.0001 cd/m².
    min_display_mastering_luminance: u16,
    /// The brightest pixel in the content, in cd/m².
    max_cll: u16,
    /// The brightest frame average, likewise. 0 is "not stated".
    max_fall: u16,
}

/// `struct hdr_output_metadata`, the blob `HDR_OUTPUT_METADATA` takes.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct HdrOutputMetadata {
    metadata_type: u32,
    hdmi_metadata_type1: HdrMetadataInfoframe,
    /// The two bytes alignment adds after the 26-byte infoframe, spelled out
    /// so they are written rather than left as whatever the stack held. The
    /// kernel ignores them; [`cast`] does not, and a blob is no place for two
    /// bytes of this process's memory.
    _tail: [u8; 2],
}

/// The kernel rejects a blob that is not exactly this structure, so its size
/// is checked here rather than at three in the morning on somebody's console.
const _: () = assert!(std::mem::size_of::<HdrOutputMetadata>() == 32);

impl HdrOutputMetadata {
    fn st2084(sdr_brightness: u16, peak: u16, display: &Display) -> Self {
        Self {
            metadata_type: 0,
            hdmi_metadata_type1: HdrMetadataInfoframe {
                eotf: EOTF_ST2084,
                metadata_type: 0,
                // BT.2020's own primaries: what the CTM has just converted
                // into, and so what the picture was in fact "mastered" on.
                display_primaries: [[35400, 14600], [8500, 39850], [6550, 2300]],
                white_point: [15635, 16450],
                max_display_mastering_luminance: peak,
                min_display_mastering_luminance: display
                    .min_luminance
                    .map(|nits| (nits * 10_000.0).round().clamp(0.0, u16::MAX as f32) as u16)
                    .unwrap_or(1),
                // Truthful rather than conventional. Nothing LineXinBar
                // composites is brighter than white, so telling the display
                // the content peaks at the SDR level lets it skip a tone
                // mapping pass it would otherwise do against the mastering
                // peak above.
                max_cll: sdr_brightness,
                max_fall: 0,
            },
            _tail: [0; 2],
        }
    }

    /// The infoframe that says "this signal is ordinary SDR again".
    ///
    /// Everything but the EOTF is zero, which is what the standard asks of a
    /// source with nothing to declare: no mastering display, no content light
    /// levels, and a transfer function the sink already has. Its whole job is
    /// to be an infoframe that arrives and says SDR, because the alternative —
    /// no infoframe at all — leaves the sink to guess. See
    /// [`Pipeline::stage_reset`].
    fn sdr() -> Self {
        Self {
            metadata_type: 0,
            hdmi_metadata_type1: HdrMetadataInfoframe {
                eotf: EOTF_SDR,
                metadata_type: 0,
                ..HdrMetadataInfoframe::default()
            },
            _tail: [0; 2],
        }
    }
}

/// `HDMI_EOTF_SMPTE_ST2084`.
const EOTF_ST2084: u8 = 2;

/// `HDMI_EOTF_TRADITIONAL_GAMMA_SDR` — the transfer function every display
/// decodes by default, and so the one that says HDR is over.
const EOTF_SDR: u8 = 0;

/// sRGB's electro-optical transfer function: coded value to linear light.
fn srgb_to_linear(coded: f32) -> f32 {
    if coded <= 0.040_45 {
        coded / 12.92
    } else {
        ((coded + 0.055) / 1.055).powf(2.4)
    }
}

/// The inverse: linear light back to an sRGB coded value.
///
/// For the night light on a display that is not in HDR. There the gamma stage
/// is the only stage — nothing has decoded its input and nothing will encode
/// its output — so a curve that scales light has to put the picture back into
/// the transfer function the display is expecting it in.
fn linear_to_srgb(light: f32) -> f32 {
    if light <= 0.003_130_8 {
        light * 12.92
    } else {
        1.055 * light.powf(1.0 / 2.4) - 0.055
    }
}

/// The inverse of SMPTE ST 2084: absolute luminance in cd/m² to a PQ code.
///
/// PQ's domain runs to 10 000 cd/m², which is why this takes nits rather than
/// a normalised value — the whole point of the curve is that a code means the
/// same luminance whatever display is reading it.
fn pq_encode(nits: f32) -> f32 {
    const M1: f32 = 2610.0 / 16384.0;
    const M2: f32 = 2523.0 / 4096.0 * 128.0;
    const C1: f32 = 3424.0 / 4096.0;
    const C2: f32 = 2413.0 / 4096.0 * 32.0;
    const C3: f32 = 2392.0 / 4096.0 * 32.0;

    let y = (nits / 10_000.0).clamp(0.0, 1.0).powf(M1);
    ((C1 + C2 * y) / (1.0 + C3 * y)).powf(M2)
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithay::output::{PhysicalProperties, Subpixel};

    fn display(name: &str) -> Output {
        Output::new(
            name.to_string(),
            PhysicalProperties {
                size: (600, 340).into(),
                subpixel: Subpixel::Unknown,
                make: "test".into(),
                model: "test".into(),
            },
        )
    }

    /// Passthrough is asked every frame and answered by a property commit, so
    /// a steady answer has to cost nothing: only a *change* may mark the
    /// display pending, or a game would re-commit the colour pipeline sixty
    /// times a second.
    #[test]
    fn only_a_change_of_passthrough_is_worth_a_commit() {
        let output = display("test-1");
        let mut manager = Manager::default();
        manager.register(
            &output,
            Settings::default(),
            NightLight::default(),
            Status::default(),
        );
        // Registering already leaves it pending; take that.
        assert!(manager.take_pending(&output).is_some());
        manager.applied(&output, Applied::default());

        assert!(manager.request_passthrough(&output, true));
        let (_, _, passthrough) = manager.take_pending(&output).expect("pending");
        assert!(passthrough);
        manager.applied(&output, Applied::default());

        // Saying the same thing again is not news.
        assert!(!manager.request_passthrough(&output, true));
        assert!(manager.take_pending(&output).is_none());

        assert!(manager.request_passthrough(&output, false));
        let (_, _, passthrough) = manager.take_pending(&output).expect("pending");
        assert!(!passthrough);
    }

    /// A display nobody registered — a nested session's — must not be a panic
    /// or a phantom entry when a fullscreen game asks for passthrough on it.
    #[test]
    fn passthrough_on_an_unknown_display_is_simply_nothing() {
        let mut manager = Manager::default();
        assert!(!manager.request_passthrough(&display("absent"), true));
    }

    /// The bug that took a machine down, written as a test so it cannot come
    /// back.
    ///
    /// `GAMMA_LUT_SIZE` and `DEGAMMA_LUT_SIZE` are immutable range properties
    /// declared over `0..=u32::MAX`, and the number of entries the hardware
    /// wants is their *value* — 4096 on the amdgpu this was found on. The
    /// first version of this module read the range's upper bound instead, and
    /// built a curve of 4 294 967 295 entries: 34 GB of allocation, filled one
    /// entry at a time, from inside the render loop of a compositor holding
    /// the console. The machine went to swap and never came back.
    ///
    /// So: the real figure is accepted, and nothing near the range bound is,
    /// whatever a driver claims.
    #[test]
    fn a_lut_is_sized_by_what_the_property_says_not_by_what_it_could_say() {
        // What the hardware this was found on actually reports.
        assert_eq!(lut_entries(4096), Some(4096));
        // The number that was read instead. 34 GB, at eight bytes an entry.
        assert_eq!(lut_entries(u32::MAX as u64), None);
        assert_eq!(lut_entries(u64::MAX), None);

        // The other sizes display engines are known to ask for.
        for size in [256, 512, 1024, 4096, 8192] {
            assert_eq!(lut_entries(size), Some(size as usize), "{size}");
        }

        // A curve needs two points, and the arithmetic that fills one divides
        // by one less than this — so one entry and none are both refused
        // rather than clamped up into a division by zero.
        assert_eq!(lut_entries(0), None);
        assert_eq!(lut_entries(1), None);
        assert_eq!(lut_entries(2), Some(2));

        // The ceiling holds, and nothing that gets past it can cost more than
        // a megabyte — the property that makes building one of these safe to
        // do on a render path at all.
        let largest = lut_entries(64 * 1024).expect("the ceiling itself is allowed");
        assert_eq!(lut_entries(64 * 1024 + 1), None, "and one past it is not");
        assert!(
            largest * std::mem::size_of::<ColorLut>() <= 1 << 20,
            "a LUT may not exceed a megabyte, got {largest} entries"
        );
    }

    /// The one number the kernel checks before it will look at the blob at
    /// all. Also asserted at compile time; this is the failure explained in
    /// words.
    #[test]
    fn the_metadata_blob_is_the_size_the_kernel_demands() {
        assert_eq!(std::mem::size_of::<HdrOutputMetadata>(), 32);
        assert_eq!(std::mem::size_of::<ColorLut>(), 8);
        assert_eq!(std::mem::size_of::<ColorCtm>(), 72);
    }

    /// Leaving HDR is an infoframe of its own, not the absence of one.
    ///
    /// A display was left black by a reset that only stopped sending the HDR
    /// infoframe: an HDMI sink that stops hearing about HDR has not been told
    /// the picture went back to SDR, and one that keeps decoding PQ shows
    /// nothing usable. The way out is the same infoframe carrying the
    /// traditional-gamma EOTF, which is what CTA-861.3 has a source send.
    #[test]
    fn the_way_out_of_hdr_is_an_sdr_infoframe() {
        let sdr = HdrOutputMetadata::sdr();
        assert_eq!(sdr.hdmi_metadata_type1.eotf, EOTF_SDR);
        assert_ne!(EOTF_SDR, EOTF_ST2084, "the two must not be the same signal");

        // Static Metadata Type 1, in both places the kernel spells it.
        assert_eq!(sdr.metadata_type, 0);
        assert_eq!(sdr.hdmi_metadata_type1.metadata_type, 0);

        // Nothing else declared: no mastering display, no content light
        // levels. The EOTF is the whole message.
        let blob = [sdr];
        let bytes = cast(&blob);
        assert_eq!(bytes.len(), 32);
        assert_eq!(
            bytes.iter().filter(|byte| **byte != 0).count(),
            0,
            "an SDR infoframe is zero throughout, EOTF included"
        );

        // And the one it replaces is not, so the sink can tell them apart.
        let display = Display {
            st2084: true,
            max_luminance: Some(600),
            ..Display::default()
        };
        let hdr = HdrOutputMetadata::st2084(200, 600, &display);
        assert_eq!(hdr.hdmi_metadata_type1.eotf, EOTF_ST2084);
        assert!(cast(&[hdr]).iter().any(|byte| *byte != 0));
    }

    /// The two ends of the transfer function, at the two luminances that have
    /// names: PQ's floor and its 10 000 cd/m² ceiling.
    #[test]
    fn the_pq_curve_spans_its_whole_range() {
        assert!(pq_encode(0.0).abs() < 1e-6);
        assert!((pq_encode(10_000.0) - 1.0).abs() < 1e-6);
        // The reference the standard is usually quoted at: 100 cd/m² sits
        // just over half way up the curve, which is what makes PQ worth
        // having — half the codes are spent below SDR white.
        let reference = pq_encode(100.0);
        assert!((0.50..0.58).contains(&reference), "{reference}");
        // And it is monotonic, or a gamma LUT built from it would fold.
        let mut previous = -1.0;
        for step in 0..=100 {
            let value = pq_encode(step as f32 * 100.0);
            assert!(value > previous, "not increasing at {step}");
            previous = value;
        }
    }

    /// sRGB's curve at the three points its definition pins down.
    #[test]
    fn srgb_decodes_to_linear_light() {
        assert_eq!(srgb_to_linear(0.0), 0.0);
        assert!((srgb_to_linear(1.0) - 1.0).abs() < 1e-6);
        // Mid grey: the whole reason a degamma stage is needed at all.
        assert!((srgb_to_linear(0.5) - 0.2140).abs() < 1e-3);
    }

    /// And back again, because the SDR night light curve makes the round trip
    /// on every entry: decode, scale, encode. A mismatched pair of transfer
    /// functions there would shift every midtone on a display whose filter is
    /// switched off in all but name.
    #[test]
    fn srgb_encodes_light_back_to_the_codes_it_came_from() {
        for step in 0..=64 {
            let coded = step as f32 / 64.0;
            let round_trip = linear_to_srgb(srgb_to_linear(coded));
            assert!(
                (round_trip - coded).abs() < 1e-5,
                "at {coded}: {round_trip}"
            );
        }
        // The two ends, and the knee where the linear segment meets the power
        // law — the one place a wrong constant hides.
        assert_eq!(linear_to_srgb(0.0), 0.0);
        assert!((linear_to_srgb(1.0) - 1.0).abs() < 1e-6);
        assert!((linear_to_srgb(0.0031308) - 0.04045).abs() < 1e-4);
    }

    /// Daylight is *exactly* no filter, and that is the one thing about this
    /// curve that has to be exact rather than close.
    ///
    /// The fit it is built on does not reach white on its own — it leaves green
    /// and blue about a percent short at 6500 K — so a night light written
    /// straight off it would tint a display that had been asked for no tint at
    /// all, with nothing in the settings able to take it back off.
    #[test]
    fn daylight_is_the_identity_and_warmer_only_takes_away() {
        let neutral = NightLight {
            enabled: true,
            temperature: NEUTRAL_KELVIN,
        };
        assert_eq!(neutral.gains(), [1.0, 1.0, 1.0]);
        assert!(!neutral.tints(), "at daylight there is nothing to commit");

        // Switched off is the identity whatever the temperature says, so a
        // remembered setting cannot leak into a display nobody warmed.
        let off = NightLight {
            enabled: false,
            temperature: 2000,
        };
        assert_eq!(off.gains(), [1.0, 1.0, 1.0]);
        assert!(!off.tints());

        // Red is never touched: warming is subtraction, so nothing clips and
        // no display is made brighter than it was.
        for kelvin in [WARMEST_KELVIN, 2000, 2700, 3400, 4000, 5000, 5500] {
            let gains = NightLight {
                enabled: true,
                temperature: kelvin,
            }
            .gains();
            assert_eq!(gains[0], 1.0, "red moved at {kelvin} K");
            assert!(gains[1] > 0.0 && gains[1] < 1.0, "{kelvin} K: {gains:?}");
            assert!(gains[2] >= 0.0 && gains[2] < 1.0, "{kelvin} K: {gains:?}");
            // Warm means blue is taken further than green, or it is a dimmer
            // rather than a filter.
            assert!(gains[2] < gains[1], "{kelvin} K is not warm: {gains:?}");
        }
    }

    /// Warmer is monotonically warmer, and out-of-range numbers land on the
    /// ends rather than anywhere surprising.
    #[test]
    fn the_filter_deepens_all_the_way_down() {
        let gains = |kelvin| {
            NightLight {
                enabled: true,
                temperature: kelvin,
            }
            .gains()
        };
        let mut previous = [1.0, 1.0, 1.0];
        for kelvin in (WARMEST_KELVIN..=NEUTRAL_KELVIN).rev().step_by(100) {
            let this = gains(kelvin);
            assert!(this[1] <= previous[1], "green rose at {kelvin} K");
            assert!(this[2] <= previous[2], "blue rose at {kelvin} K");
            previous = this;
        }

        // Clamped, not wrapped or refused: a shell asking for something out of
        // range gets the nearest picture that means anything.
        assert_eq!(gains(0), gains(WARMEST_KELVIN));
        assert_eq!(gains(u16::MAX), gains(NEUTRAL_KELVIN));
        // And at the warm end the blue is gone entirely, which is where the
        // logarithm in the fit would otherwise run off.
        assert_eq!(gains(WARMEST_KELVIN)[2], 0.0);
    }

    /// The two stages describe one white point. The SDR filter works in sRGB
    /// codes and the HDR one in linear light, and a session that turned HDR on
    /// must not see its night light change colour as it does.
    #[test]
    fn both_encodings_of_the_filter_are_the_same_white_point() {
        for kelvin in [2000u16, 2700, 3400, 4000, 5000] {
            let night = NightLight {
                enabled: true,
                temperature: kelvin,
            };
            let coded = night.gains();
            let light = night.linear_gains();
            for channel in 0..3 {
                // The linear gain is the coded one decoded, which is what
                // makes `(c·g)^γ = c^γ·g^γ` hold across the two stages.
                let expected = srgb_to_linear(coded[channel]);
                assert!(
                    (light[channel] - expected).abs() < 1e-6,
                    "{kelvin} K channel {channel}: {light:?} vs {coded:?}"
                );
                // And the darker channel stays the darker one in both.
                assert!(light[channel] <= 1.0);
            }
        }
    }

    /// A ramp entry is three numbers now, and the grey helper still has to
    /// produce three equal ones — every HDR curve is a tone curve.
    #[test]
    fn a_ramp_entry_carries_three_channels() {
        let grey = ColorLut::grey(0.5);
        assert_eq!(grey.red, grey.green);
        assert_eq!(grey.green, grey.blue);
        assert_eq!(ColorLut::grey(1.0).red, u16::MAX);
        assert_eq!(ColorLut::grey(0.0).red, 0);

        let warm = ColorLut::rgb(1.0, 0.8, 0.6);
        assert_eq!(warm.red, u16::MAX);
        assert!(warm.green < warm.red && warm.blue < warm.green);
        // Out of range in either direction is clamped rather than wrapped: a
        // ramp entry that overflowed would be a black pixel where the brightest
        // one belongs.
        assert_eq!(ColorLut::rgb(2.0, -1.0, 0.0).red, u16::MAX);
        assert_eq!(ColorLut::rgb(2.0, -1.0, 0.0).green, 0);
        assert_eq!(warm.reserved, 0);
    }

    /// Both ends of the intensity slider, and the property that makes the
    /// blend between them safe: white is white at every setting, because every
    /// row sums to one.
    #[test]
    fn the_gamut_matrix_never_moves_white() {
        for intensity in [0u8, 25, 50, 75, 100, 200] {
            let ctm = ColorCtm::gamut(intensity);
            for row in 0..3 {
                let sum: f64 = (0..3)
                    .map(|column| ctm.matrix[row * 3 + column] as f64 / FIXED_POINT)
                    .sum();
                assert!((sum - 1.0).abs() < 1e-6, "row {row} at {intensity}: {sum}");
            }
        }

        // At 100 it is the identity, so the display is handed sRGB's numbers
        // as BT.2020's — the vivid end.
        let vivid = ColorCtm::gamut(100);
        for row in 0..3 {
            for column in 0..3 {
                let expected = if row == column { FIXED_POINT } else { 0.0 };
                assert_eq!(vivid.matrix[row * 3 + column] as f64, expected);
            }
        }

        // At 0 it is the real conversion, which pulls every primary inwards.
        let exact = ColorCtm::gamut(0);
        assert!((exact.matrix[0] as f64 / FIXED_POINT - 0.6274).abs() < 1e-3);
    }

    /// Three answers about the gamut, not two.
    ///
    /// A matrix behind a degamma stage is the conversion. A matrix with
    /// nothing in front of it is an approximation of it, and it is offered,
    /// because the alternative — leaving the matrix out — is the *identity*,
    /// and the identity is the most saturated end of the very setting being
    /// refused. No matrix at all is the only case with nothing to offer.
    #[test]
    fn a_matrix_with_no_linear_stage_still_converts() {
        let handle =
            |id: u32| property::Handle::from(std::num::NonZeroU32::new(id).expect("not zero"));

        let whole = Pipeline {
            degamma: Some((handle(1), 4096)),
            ctm: Some(handle(2)),
            ..Default::default()
        };
        assert!(whole.converts_gamut());
        assert!(whole.converts_gamut_exactly());
        assert_eq!(whole.describe(), "full pipeline");

        // What every RDNA 4 card reports: amdgpu withholds DEGAMMA_LUT on
        // DCN 4.01 because a pre-blending degamma would not apply to the
        // cursor.
        let matrix_only = Pipeline {
            ctm: Some(handle(2)),
            ..Default::default()
        };
        assert!(matrix_only.converts_gamut());
        assert!(!matrix_only.converts_gamut_exactly());
        assert!(matrix_only.describe().contains("approximately"));

        // Nowhere to put it.
        let neither = Pipeline::default();
        assert!(!neither.converts_gamut());
        assert!(!neither.converts_gamut_exactly());
        assert!(neither.describe().contains("BT.2020"));

        // And a linear stage with nothing to act on the output of it is the
        // same answer as no pipeline at all.
        let degamma_only = Pipeline {
            degamma: Some((handle(1), 4096)),
            ..Default::default()
        };
        assert!(!degamma_only.converts_gamut());
        assert!(!degamma_only.converts_gamut_exactly());
    }

    /// An EDID from a display that claims HDR, assembled the way a real one
    /// is: a base block saying there is one extension, and a CTA extension
    /// carrying the colorimetry and HDR static metadata blocks.
    /// An EDID claiming HDR, with `peak` as its luminance code.
    ///
    /// Assembled here rather than captured off a monitor: what is under test
    /// is the decode, and a real display's bytes would tie it to one piece of
    /// hardware nobody else has.
    fn hdr_edid(peak: u8) -> Vec<u8> {
        let mut edid = vec![0u8; 256];
        edid[126] = 1;

        let cta = &mut edid[128..];
        cta[0] = CTA_EXTENSION_TAG;
        cta[1] = 3;
        // Colorimetry: extended tag 5, BT.2020 RGB.
        cta[4] = (CTA_TAG_EXTENDED << 5) | 3;
        cta[5] = CTA_EXTENDED_COLORIMETRY;
        cta[6] = 0x80;
        cta[7] = 0;
        // HDR static metadata: extended tag 6, ST 2084, with luminances.
        cta[8] = (CTA_TAG_EXTENDED << 5) | 6;
        cta[9] = CTA_EXTENDED_HDR_STATIC;
        cta[10] = 0x06; // traditional HDR gamma and ST 2084
        cta[11] = 0x01;
        cta[12] = peak;
        cta[13] = 90; // frame average
        cta[14] = 40; // black
                      // Detailed timings start after both blocks.
        cta[2] = 15;
        edid
    }

    #[test]
    fn an_hdr_display_is_recognised_by_its_edid() {
        let display = Display::from_edid(&hdr_edid(114));
        assert!(display.st2084, "the display accepts PQ");
        assert!(display.bt2020, "and BT.2020 RGB");
        assert!(display.min_luminance.unwrap() < 0.2);

        // The CTA's luminance scale, stated as the property that defines it
        // rather than as one number off one monitor: 50 cd/m² at code 0, and a
        // doubling every 32 codes after that.
        let peak = |code| Display::from_edid(&hdr_edid(code)).max_luminance;
        assert_eq!(peak(0), Some(50));
        assert_eq!(peak(32), Some(100));
        assert_eq!(peak(64), Some(200));
        assert_eq!(peak(96), Some(400));
        assert_eq!(peak(128), Some(800));
        // And it is monotonic in between, across the whole byte.
        let mut previous = 0;
        for code in 0..=u8::MAX {
            let this = peak(code).expect("every code decodes");
            assert!(this >= previous, "code {code} went backwards");
            previous = this;
        }
    }

    /// The three ways an EDID says nothing about HDR, none of which may come
    /// back as a display that can be driven in it — and none of which may
    /// panic, because an EDID is bytes off a cable.
    #[test]
    fn a_display_that_says_nothing_claims_nothing() {
        assert_eq!(Display::from_edid(&[]), Display::default());
        // An SDR display: base block, one CTA extension, no HDR block.
        let mut sdr = vec![0u8; 256];
        sdr[126] = 1;
        sdr[128] = CTA_EXTENSION_TAG;
        sdr[130] = 4;
        assert_eq!(Display::from_edid(&sdr), Display::default());
        // An EDID that claims an extension it does not carry.
        let mut truncated = vec![0u8; 128];
        truncated[126] = 4;
        assert_eq!(Display::from_edid(&truncated), Display::default());
        // A data block whose length runs off the end of its own block.
        let mut ragged = vec![0u8; 256];
        ragged[126] = 1;
        ragged[128] = CTA_EXTENSION_TAG;
        ragged[130] = 127;
        ragged[132] = (CTA_TAG_EXTENDED << 5) | 31;
        assert_eq!(Display::from_edid(&ragged), Display::default());
    }

    /// The peak is never allowed below the level white is being sent at: the
    /// gamma curve clamps to it, so a peak under white would flatten every
    /// highlight into one code.
    #[test]
    fn the_declared_peak_is_never_below_white() {
        let display = Display {
            st2084: true,
            max_luminance: Some(300),
            ..Display::default()
        };
        let bright = Settings {
            sdr_brightness: 400,
            ..Settings::default()
        };
        assert_eq!(bright.peak(&display), 400);
        assert_eq!(Settings::default().peak(&display), 300);
        // Nothing said anywhere: the modest assumption, not zero.
        assert_eq!(Settings::default().peak(&Display::default()), DEFAULT_PEAK);
        // What the user says beats what the display claims.
        let forced = Settings {
            peak_brightness: Some(1000),
            ..Settings::default()
        };
        assert_eq!(forced.peak(&display), 1000);
    }
}
