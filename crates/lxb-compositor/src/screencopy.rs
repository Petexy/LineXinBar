//! `wlr-screencopy-unstable-v1`: the standard way anything else reads this
//! session's screen.
//!
//! [`crate::capture`] photographs a display for the shell, over the session's
//! own private protocol. That is enough for a screenshot key and no use at all
//! to a recorder, a browser sharing a tab, or the desktop portal every one of
//! them actually goes through: those speak a protocol they already know, and
//! until this existed they saw a black screen and reported no error.
//!
//! What a client does here is ask for one frame of one display, get told what
//! buffer to bring, and hand that buffer over. The compositor fills it the next
//! time it draws that display, and says when it is done. A recording is that,
//! repeated — ask, copy, ask again — which is also what paces it: a frame is
//! only answered when the display it belongs to actually draws, so a recorder
//! runs at the screen's rate without having to guess what that is.
//!
//! ## What lands in the buffer
//!
//! The composite, exactly as [`crate::capture::output`] takes it: the same
//! element list the display draws, at the size the display *shows* rather than
//! the size the connector scans out, so a screen standing on its side is
//! recorded the way somebody looking at it sees it.
//!
//! The cursor is the one difference, and it is the client's to choose. A
//! screenshot has no business containing an arrow the user cannot remove; a
//! recording of somebody demonstrating something is nearly useless without one.
//! That is what `overlay_cursor` is, and why the cursor is decided here rather
//! than once and for all.
//!
//! ## Who may
//!
//! Any client of this compositor, which is the footing `lxb_shell_v1` and
//! wlr-layer-shell are already on: the session only ever runs what the user
//! started. Consent does not belong at this level — the portal above it is what
//! asks the user which screen an application may see, and an application that
//! could reach past the portal to here is one that has already been handed the
//! session's own socket.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::element::utils::{Relocate, RelocateRenderElement};
use smithay::backend::renderer::element::RenderElement;
use smithay::backend::renderer::gles::GlesTexture;
use smithay::backend::renderer::{ExportMem, ImportAll, ImportMem, Offscreen, Renderer};
use smithay::output::{Output, OutputModeSource};
use smithay::reexports::wayland_protocols_wlr::screencopy::v1::server::zwlr_screencopy_frame_v1::{
    self, ZwlrScreencopyFrameV1,
};
use smithay::reexports::wayland_protocols_wlr::screencopy::v1::server::zwlr_screencopy_manager_v1::{
    self, ZwlrScreencopyManagerV1,
};
use smithay::reexports::wayland_server::backend::{ClientId, GlobalId, ObjectId};
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_shm;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};
use smithay::utils::{Logical, Physical, Rectangle, Size, Transform};
use smithay::wayland::shm::with_buffer_contents_mut;

use crate::render::CursorState;
use crate::state::{Lxb, LxbState};

/// The version advertised. Three is the last there is: it adds the buffer_done
/// handshake, which is what lets a compositor offer more than one kind of
/// buffer — and, by being answered, tells this one that the client understood
/// the offer.
const VERSION: u32 = 3;

/// The format every frame is offered in.
///
/// One format, deliberately. `Xbgr8888` is eight bits a channel in the order
/// the renderer already reads them back — R, G, B, then a byte the screen has
/// no use for — so the copy out of the GPU is the copy into the client's
/// buffer, with nothing swizzled on the way past. Four formats would mean four
/// paths through here and three of them exercised by nobody.
///
/// An `X` format rather than an `A` one because a screen has no alpha: what is
/// behind the display is not a question anybody is asking.
pub const FORMAT: wl_shm::Format = wl_shm::Format::Xbgr8888;

/// The same order with the fourth byte meant: what a picture of one *layer* of
/// the screen is written in.
///
/// A screen has no alpha and a layer of one is nothing but: the shell
/// composites the picture over the wallpaper it evaluates for itself, so what
/// nothing covers has to arrive covering nothing. See
/// `lxb_shell_v1.ask_for_the_picture_behind`.
pub const LAYER_FORMAT: wl_shm::Format = wl_shm::Format::Abgr8888;

/// What one frame is a picture of.
#[derive(Debug)]
struct Target {
    output: Output,
    /// Which part of that display, in its own shown pixels. All of it, for a
    /// `capture_output`.
    region: Rectangle<i32, Physical>,
    overlay_cursor: bool,
    /// The manager the frame came from. Damage is answered per manager — "what
    /// has changed since the last frame *you* took" — so a recorder and a
    /// screenshot tool running at the same time do not consume each other's.
    manager: ObjectId,
}

/// A frame object's state: what it is a picture of, and whether it has been
/// spent.
#[derive(Debug)]
pub struct FrameState {
    /// `None` for a frame that was born failed, because the display it named
    /// had gone or was being driven at no mode.
    target: Option<Target>,
    /// A frame may be copied once. A second `copy` is a protocol error rather
    /// than something to be tolerated, so this is checked.
    used: AtomicBool,
}

impl FrameState {
    fn failed() -> Self {
        Self {
            target: None,
            used: AtomicBool::new(true),
        }
    }

    fn output(&self) -> Option<&Output> {
        self.target.as_ref().map(|target| &target.output)
    }
}

/// A frame with a buffer under it, waiting for its display to draw.
#[derive(Debug)]
struct Pending {
    frame: ZwlrScreencopyFrameV1,
    buffer: WlBuffer,
    /// Whether the client asked to be kept waiting until something changes.
    /// That is what `copy_with_damage` is for: a recorder taking a frame of a
    /// still screen sixty times a second is encoding one picture sixty times.
    with_damage: bool,
}

/// Everything the compositor holds on screencopy's behalf.
pub struct ScreencopyState {
    #[allow(dead_code)]
    global: GlobalId,
    /// Frames waiting for the display they belong to.
    pending: Vec<Pending>,
    /// One damage tracker per manager per display, so "what has changed since
    /// your last frame" has an answer. Nothing is rendered through these; they
    /// are asked what moved and nothing else.
    damage: Vec<(ObjectId, Output, OutputDamageTracker)>,
}

impl std::fmt::Debug for ScreencopyState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScreencopyState")
            .field("pending", &self.pending.len())
            .field("tracked", &self.damage.len())
            .finish()
    }
}

impl ScreencopyState {
    pub fn new<D>(display: &DisplayHandle) -> Self
    where
        D: GlobalDispatch<ZwlrScreencopyManagerV1, ()> + 'static,
    {
        Self {
            global: display.create_global::<D, ZwlrScreencopyManagerV1, _>(VERSION, ()),
            pending: Vec::new(),
            damage: Vec::new(),
        }
    }

    /// Whether anything is waiting on `output`. Asked by a backend before it
    /// goes to the trouble of assembling the screen a second time.
    pub fn wanted(&self, output: &Output) -> bool {
        self.pending
            .iter()
            .any(|pending| frame_is_for(pending, output))
    }

    /// Take the frames waiting on `output`, leaving the rest where they are.
    fn take(&mut self, output: &Output) -> Vec<Pending> {
        let mut taken = Vec::new();
        let mut kept = Vec::new();
        for pending in self.pending.drain(..) {
            if frame_is_for(&pending, output) {
                taken.push(pending);
            } else {
                kept.push(pending);
            }
        }
        self.pending = kept;
        taken
    }

    /// The damage tracker for one manager's view of one display, made on first
    /// use.
    fn tracker(&mut self, manager: &ObjectId, output: &Output) -> &mut OutputDamageTracker {
        let known = self
            .damage
            .iter()
            .position(|(id, seen, _)| id == manager && seen == output);
        let index = match known {
            Some(index) => index,
            None => {
                self.damage.push((
                    manager.clone(),
                    output.clone(),
                    OutputDamageTracker::from_mode_source(mode_source(output)),
                ));
                self.damage.len() - 1
            }
        };
        &mut self.damage[index].2
    }

    /// Forget everything belonging to a display that has gone away.
    ///
    /// Its frames are failed rather than dropped: a client waiting on a copy
    /// that will never come is a recorder that hangs, and an unplugged monitor
    /// is not its fault.
    pub fn output_gone(&mut self, output: &Output) {
        let (gone, kept): (Vec<_>, Vec<_>) = self
            .pending
            .drain(..)
            .partition(|pending| frame_is_for(pending, output));
        self.pending = kept;
        for pending in gone {
            pending.frame.failed();
        }
        self.damage.retain(|(_, seen, _)| seen != output);
    }
}

fn frame_is_for(pending: &Pending, output: &Output) -> bool {
    pending
        .frame
        .data::<FrameState>()
        .and_then(FrameState::output)
        == Some(output)
}

/// How big the picture of `output` is: the size that display *shows*, in its
/// own pixels.
///
/// Not the mode's size. A panel driven at 1920×1080 and turned a quarter turn
/// shows a 1080×1920 picture, everything on it is laid out in that space, and
/// that is the space a copy of it is measured in. `None` for a display being
/// driven at no mode at all, which has no picture to copy.
pub fn picture_size(output: &Output) -> Option<Size<i32, Physical>> {
    let mode = output.current_mode()?;
    if mode.size.w <= 0 || mode.size.h <= 0 {
        return None;
    }
    // The transform says how the contents are turned to reach the panel, so
    // undoing it takes the panel's frame back to the picture's own shape.
    let size = output
        .current_transform()
        .invert()
        .transform_size(mode.size);
    Some(Size::from((size.w, size.h)))
}

/// The mode a copy of `output` is drawn against: its shown size, its scale, and
/// no turn.
///
/// The turn is deliberately dropped. It has already been accounted for by the
/// time the picture is this shape — [`picture_size`] *is* the turned size — and
/// applying it a second time would lay the contents back on their side inside
/// their own upright frame.
fn mode_source(output: &Output) -> OutputModeSource {
    OutputModeSource::Static {
        size: picture_size(output).unwrap_or_default(),
        scale: output.current_scale().fractional_scale().into(),
        transform: Transform::Normal,
    }
}

/// Fill in every frame waiting on `output` and tell its client.
///
/// Called by a backend once it has drawn that display, because that is where a
/// live renderer is and when the display's contents are what a client asked for
/// a picture *of*. Everything that can fail here fails one frame rather than
/// the pass: a client with a bad buffer must not cost the display a frame, let
/// alone the session.
pub fn serve<R>(
    renderer: &mut R,
    lxb: &mut Lxb,
    output: &Output,
    cursor: Option<&mut CursorState>,
    time: Duration,
) where
    R: Renderer + ImportAll + ImportMem + ExportMem + Offscreen<GlesTexture>,
    R::TextureId: Send + Clone + 'static,
    R::Error: Send + Sync + 'static,
{
    let waiting = lxb.screencopy.take(output);
    if waiting.is_empty() {
        return;
    }
    if picture_size(output).is_none() {
        for pending in waiting {
            pending.frame.failed();
        }
        return;
    }

    // At most two element lists, because there are only two answers to whether
    // the cursor is in the picture — and usually one, since a session is not
    // normally being recorded and photographed at the same moment.
    let asked = |want: bool| {
        waiting.iter().any(|pending| {
            pending
                .frame
                .data::<FrameState>()
                .and_then(|frame| frame.target.as_ref())
                .is_some_and(|target| target.overlay_cursor == want)
        })
    };
    let with_cursor = if asked(true) {
        Some(crate::render::output_elements(
            renderer, lxb, output, cursor,
        ))
    } else {
        None
    };
    let without_cursor =
        asked(false).then(|| crate::render::output_elements(renderer, lxb, output, None));

    let clear = lxb.config.general.background;
    for pending in waiting {
        let Some(target) = pending
            .frame
            .data::<FrameState>()
            .and_then(|frame| frame.target.as_ref())
        else {
            pending.frame.failed();
            continue;
        };
        let elements = if target.overlay_cursor {
            with_cursor.as_deref()
        } else {
            without_cursor.as_deref()
        };
        let Some(elements) = elements else {
            pending.frame.failed();
            continue;
        };

        // What has changed since this client's last frame — both the answer to
        // copy_with_damage and what decides whether to answer it at all.
        let changed: Option<Vec<Rectangle<i32, Physical>>> = pending.with_damage.then(|| {
            lxb.screencopy
                .tracker(&target.manager, output)
                .damage_output(1, elements)
                .ok()
                .and_then(|(damage, _)| damage.cloned())
                .unwrap_or_default()
        });
        if changed.as_ref().is_some_and(|damage| damage.is_empty()) {
            // Nothing moved. The frame goes back on the queue and is asked
            // again the next time this display draws, which is what "wait until
            // there is damage" means.
            lxb.screencopy.pending.push(pending);
            continue;
        }

        match copy_into(renderer, target.region, elements, &pending.buffer, clear) {
            Ok(()) => {
                // Nothing is upside down: the renderer draws into the buffer
                // top row first and it is read back the same way.
                pending
                    .frame
                    .flags(zwlr_screencopy_frame_v1::Flags::empty());
                for rectangle in changed.into_iter().flatten() {
                    pending.frame.damage(
                        rectangle.loc.x.max(0) as u32,
                        rectangle.loc.y.max(0) as u32,
                        rectangle.size.w.max(0) as u32,
                        rectangle.size.h.max(0) as u32,
                    );
                }
                let seconds = time.as_secs();
                pending.frame.ready(
                    (seconds >> 32) as u32,
                    (seconds & 0xffff_ffff) as u32,
                    time.subsec_nanos(),
                );
            }
            Err(err) => {
                tracing::warn!(?err, display = %output.name(), "a screen copy failed");
                pending.frame.failed();
            }
        }
    }
}

/// Draw `elements` into the client's buffer.
///
/// The region is moved to the buffer's own corner rather than being cut out
/// afterwards, so a client asking for a quarter of the screen costs a quarter
/// of the screen to draw rather than a whole one and a crop.
fn copy_into<R, E>(
    renderer: &mut R,
    region: Rectangle<i32, Physical>,
    elements: &[E],
    buffer: &WlBuffer,
    clear: [f32; 4],
) -> anyhow::Result<()>
where
    R: Renderer + ImportMem + ExportMem + Offscreen<GlesTexture>,
    R::TextureId: Send + Clone + 'static,
    R::Error: Send + Sync + 'static,
    E: RenderElement<R>,
{
    let moved: Vec<RelocateRenderElement<&E>> = elements
        .iter()
        .map(|element| {
            RelocateRenderElement::from_element(
                element,
                (-region.loc.x, -region.loc.y),
                Relocate::Relative,
            )
        })
        .collect();

    // A fresh tracker every time, so every pixel of the buffer is written. The
    // damage reported to the client is advisory — it says what is worth looking
    // at again — and a client handing over a buffer it has never had back
    // would otherwise be given a frame with somebody else's picture in the
    // parts that did not move.
    let mut tracker = OutputDamageTracker::new(region.size, 1.0, Transform::Normal);
    let shot = crate::capture::shoot(renderer, region.size, &mut tracker, &moved, clear)?;

    with_buffer_contents_mut(buffer, |slot, len, data| {
        let stride = data.stride as usize;
        let width = data.width as usize;
        let height = data.height as usize;
        let offset = data.offset as usize;
        if width != shot.width as usize || height != shot.height as usize {
            anyhow::bail!("the buffer changed size between the copy request and the frame");
        }
        if offset + stride * height > len || stride < width * 4 {
            anyhow::bail!("the buffer is smaller than it says it is");
        }
        for row in 0..height {
            // Row by row, because a client's stride is its own business: it may
            // be padded, and a picture written as one run would come out
            // sheared on a buffer that is.
            let from = &shot.rgba[row * width * 4..(row + 1) * width * 4];
            // SAFETY: the pool is mapped for `len` bytes, and the write above
            // is bounded by the `offset + stride * height` check.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    from.as_ptr(),
                    slot.add(offset + row * stride),
                    width * 4,
                );
            }
        }
        Ok(())
    })??;
    Ok(())
}

impl GlobalDispatch<ZwlrScreencopyManagerV1, ()> for LxbState {
    fn bind(
        _state: &mut Self,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<ZwlrScreencopyManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<ZwlrScreencopyManagerV1, ()> for LxbState {
    fn request(
        state: &mut Self,
        _client: &Client,
        manager: &ZwlrScreencopyManagerV1,
        request: zwlr_screencopy_manager_v1::Request,
        _data: &(),
        _handle: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        use zwlr_screencopy_manager_v1::Request;
        let (frame, output, overlay_cursor, region) = match request {
            Request::CaptureOutput {
                frame,
                overlay_cursor,
                output,
            } => (frame, output, overlay_cursor, None),
            Request::CaptureOutputRegion {
                frame,
                overlay_cursor,
                output,
                x,
                y,
                width,
                height,
            } => (
                frame,
                output,
                overlay_cursor,
                Some(Rectangle::<i32, Logical>::new(
                    (x, y).into(),
                    (width, height).into(),
                )),
            ),
            Request::Destroy => return,
            _ => return,
        };

        // Still one of ours: an output resource outlives the display by however
        // long it takes its client to hear that it has gone.
        let display = Output::from_resource(&output)
            .filter(|display| state.lxb.space.outputs().any(|known| known == display));
        let Some((display, size)) =
            display.and_then(|display| picture_size(&display).map(|size| (display, size)))
        else {
            data_init.init(frame, FrameState::failed()).failed();
            return;
        };

        // A region is named in the display's own logical coordinates, from its
        // own corner — the same measure `xdg_output.logical_size` is in. Here
        // it becomes pixels, and is clipped: a client asking for more than the
        // screen holds is given the screen.
        let scale = display.current_scale().fractional_scale();
        let whole = Rectangle::from_size(size);
        let region = match region {
            Some(region) => region.to_physical_precise_round(scale).intersection(whole),
            None => Some(whole),
        };
        let Some(region) = region.filter(|region| region.size.w > 0 && region.size.h > 0) else {
            data_init.init(frame, FrameState::failed()).failed();
            return;
        };

        let frame = data_init.init(
            frame,
            FrameState {
                target: Some(Target {
                    output: display,
                    region,
                    overlay_cursor: overlay_cursor != 0,
                    manager: manager.id(),
                }),
                used: AtomicBool::new(false),
            },
        );
        frame.buffer(
            FORMAT,
            region.size.w as u32,
            region.size.h as u32,
            (region.size.w * 4) as u32,
        );
        // Version 3 clients wait to be told the list of offers is complete;
        // older ones take the one buffer event as the whole answer, which it is.
        if frame.version() >= 3 {
            frame.buffer_done();
        }
    }
}

impl Dispatch<ZwlrScreencopyFrameV1, FrameState> for LxbState {
    fn request(
        state: &mut Self,
        _client: &Client,
        frame: &ZwlrScreencopyFrameV1,
        request: zwlr_screencopy_frame_v1::Request,
        data: &FrameState,
        _handle: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        let (buffer, with_damage) = match request {
            zwlr_screencopy_frame_v1::Request::Copy { buffer } => (buffer, false),
            zwlr_screencopy_frame_v1::Request::CopyWithDamage { buffer } => (buffer, true),
            zwlr_screencopy_frame_v1::Request::Destroy => return,
            _ => return,
        };

        if data.used.swap(true, Ordering::SeqCst) {
            frame.post_error(
                zwlr_screencopy_frame_v1::Error::AlreadyUsed,
                "this frame has already been copied",
            );
            return;
        }
        let Some(target) = data.target.as_ref() else {
            // Born failed, and already told so.
            return;
        };
        if let Err(why) = suitable(&buffer, target.region.size) {
            frame.post_error(zwlr_screencopy_frame_v1::Error::InvalidBuffer, why);
            return;
        }

        state.lxb.screencopy.pending.push(Pending {
            frame: frame.clone(),
            buffer,
            with_damage,
        });
        // A display with nothing moving on it draws nothing, and a frame
        // waiting on a screen nobody is touching would wait for ever.
        state.queue_redraw();
    }

    fn destroyed(
        state: &mut Self,
        _client: ClientId,
        frame: &ZwlrScreencopyFrameV1,
        _data: &FrameState,
    ) {
        state
            .lxb
            .screencopy
            .pending
            .retain(|pending| &pending.frame != frame);
    }
}

/// Whether a client's buffer is one this frame can be copied into.
///
/// Refused rather than worked around. The size and the format were both stated
/// in the buffer event the client is answering, and a copy into a buffer that
/// disagrees with them is either a torn picture or a write past the end of a
/// mapping.
fn suitable(buffer: &WlBuffer, size: Size<i32, Physical>) -> Result<(), &'static str> {
    let checked = with_buffer_contents_mut(buffer, |_, len, data| {
        if data.format != FORMAT {
            return Err("the buffer is not in the format the frame offered");
        }
        if data.width != size.w || data.height != size.h {
            return Err("the buffer is not the size the frame offered");
        }
        if data.stride < size.w.saturating_mul(4) {
            return Err("the buffer's stride is too small for its width");
        }
        let needed = i64::from(data.offset) + i64::from(data.stride) * i64::from(data.height);
        if data.offset < 0 || needed > len as i64 {
            return Err("the buffer runs past the end of its pool");
        }
        Ok(())
    });
    match checked {
        Ok(result) => result,
        // Not shm at all: a dmabuf, or something out of a protocol this
        // compositor does not know. Only wl_shm buffers were offered.
        Err(_) => Err("only wl_shm buffers can be copied into"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithay::output::{Mode, PhysicalProperties, Scale, Subpixel};

    fn output(mode: (i32, i32), transform: Transform, scale: f64) -> Output {
        let output = Output::new(
            "test".to_string(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "test".into(),
                model: "test".into(),
            },
        );
        output.change_current_state(
            Some(Mode {
                size: mode.into(),
                refresh: 60_000,
            }),
            Some(transform),
            Some(Scale::Fractional(scale)),
            None,
        );
        output
    }

    /// The picture is what the display shows, which on a turned screen is not
    /// the frame the connector scans out.
    #[test]
    fn a_turned_display_is_copied_the_way_it_is_looked_at() {
        let landscape = output((1920, 1080), Transform::Normal, 1.0);
        assert_eq!(picture_size(&landscape), Some(Size::from((1920, 1080))));

        for turned in [Transform::_90, Transform::_270] {
            let portrait = output((1920, 1080), turned, 1.0);
            assert_eq!(
                picture_size(&portrait),
                Some(Size::from((1080, 1920))),
                "a screen on its side shows a picture on its side"
            );
        }
        // Half a turn is still landscape; only the quarters swap the sides.
        let upside_down = output((1920, 1080), Transform::_180, 1.0);
        assert_eq!(picture_size(&upside_down), Some(Size::from((1920, 1080))));
    }

    /// The size is in pixels, so a doubled display is copied at its own
    /// resolution rather than at the size it reports to clients.
    #[test]
    fn a_scaled_display_is_copied_at_its_own_pixels() {
        let doubled = output((3840, 2160), Transform::Normal, 2.0);
        assert_eq!(picture_size(&doubled), Some(Size::from((3840, 2160))));
    }

    /// A display being driven at nothing has no picture, and is refused rather
    /// than copied as an empty one.
    #[test]
    fn a_display_with_no_mode_has_no_picture() {
        let dark = Output::new(
            "dark".to_string(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "test".into(),
                model: "test".into(),
            },
        );
        assert_eq!(picture_size(&dark), None);
    }
}
