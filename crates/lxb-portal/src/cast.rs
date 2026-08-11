//! One display, going out as a PipeWire stream.
//!
//! This is the join between the two halves of screen sharing. On one side the
//! compositor hands out frames over `wlr-screencopy`, one at a time, when the
//! display it belongs to draws. On the other, every application that shares a
//! screen — OBS, a browser, Discord — reads a PipeWire node and knows nothing
//! about Wayland at all.
//!
//! ## The frame never gets copied twice
//!
//! Every buffer in the stream is a memfd this process allocates, and a memfd is
//! exactly what a `wl_shm_pool` can be made out of — so the memory PipeWire
//! hands to the consumer is the same memory the compositor is told to copy the
//! display into. The picture goes from the GPU to the application that asked
//! for it in one step, and nothing in this process ever touches a pixel.
//!
//! ## What paces it
//!
//! Nothing here. `copy_with_damage` is answered when the display draws and
//! *only* when something on it has changed, so a still screen costs one frame
//! and then silence, and a moving one arrives at the rate the screen is moving
//! at. The next frame is asked for as soon as the last one is queued, which
//! makes the compositor the clock.
//!
//! ## One loop
//!
//! PipeWire's loop is the only loop. The Wayland connection is registered on it
//! as a plain file descriptor, so screencopy events and PipeWire callbacks
//! arrive on the same thread and neither has to lock against the other.

use std::cell::RefCell;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::rc::Rc;

use pipewire as pw;
use pw::spa;
use spa::param::video::{VideoFormat, VideoInfoRaw};
use spa::pod::Pod;
use spa::utils::{Direction, Fraction, Rectangle};
use wayland_client::protocol::{wl_buffer, wl_output, wl_registry, wl_shm, wl_shm_pool};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum};
use wayland_protocols_wlr::screencopy::v1::client::zwlr_screencopy_frame_v1::{
    self, ZwlrScreencopyFrameV1,
};
use wayland_protocols_wlr::screencopy::v1::client::zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1;

/// What the portal was asked to share.
#[derive(Debug, Clone)]
pub struct Wanted {
    /// The display, by the name the compositor gives it — `DP-2`, `HDMI-A-1`.
    /// `None` takes the first one, which is what a session with one display
    /// means by "the screen".
    pub output: Option<String>,
    /// Whether the pointer is in the picture. The whole reason screencopy
    /// carries this per frame: a recording of somebody demonstrating something
    /// is nearly useless without one.
    pub cursor: bool,
}

/// How the node id is handed back to whoever asked for the cast: once, and
/// from inside the loop, because the number does not exist until PipeWire has
/// made the node.
type Announcement = Box<dyn FnOnce(Live)>;

/// What the stream turned out to be, once PipeWire and the compositor have both
/// agreed to it. Handed back to whoever asked for the cast, because the node id
/// is the only part of this an application on the other side ever sees.
#[derive(Debug, Clone, Copy)]
pub struct Live {
    pub node: u32,
    pub width: u32,
    pub height: u32,
}

/// How big one display's picture is, and how it is laid out. Everything about
/// the stream follows from this, and it comes from the compositor rather than
/// being asked for: the frame event is the first thing screencopy says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Picture {
    width: u32,
    height: u32,
    stride: u32,
}

impl Picture {
    fn size(&self) -> usize {
        self.stride as usize * self.height as usize
    }
}

/// The format the compositor offers and the one PipeWire is told about, which
/// are the same four bytes under two names: R, G, B and one the screen has no
/// use for.
const SHM_FORMAT: wl_shm::Format = wl_shm::Format::Xbgr8888;
const SPA_FORMAT: VideoFormat = VideoFormat::RGBx;

/// The rate the stream declares as its ceiling. The real rate is whatever the
/// display draws at; this is only what a consumer is told to expect.
const MAX_FRAMERATE: u32 = 60;

/// One PipeWire buffer, with the `wl_buffer` that writes into it.
struct Slot {
    pw: *mut pw::sys::pw_buffer,
    /// Held because dropping it destroys the buffer the compositor is copying
    /// into.
    #[allow(dead_code)]
    pool: wl_shm_pool::WlShmPool,
    buffer: wl_buffer::WlBuffer,
    /// The frame's own memory, and the descriptor both halves of the session
    /// reach it through. Owned here so that unmapping and closing happen when
    /// PipeWire takes the buffer back, and not before.
    memory: *mut std::ffi::c_void,
    /// Held, not read: closing it would take the frame's memory out from under
    /// both the compositor and the consumer.
    #[allow(dead_code)]
    file: rustix::fd::OwnedFd,
    size: usize,
    stride: u32,
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.buffer.destroy();
        self.pool.destroy();
        // SAFETY: this mapping was made in `add_slot` for exactly `size` bytes
        // and is not shared with anything that outlives the slot.
        unsafe {
            let _ = rustix::mm::munmap(self.memory, self.size);
        }
    }
}

/// A screencopy frame that has been asked for and not yet answered.
struct InFlight {
    frame: ZwlrScreencopyFrameV1,
    /// The buffer it is being copied into, once one has been taken out of the
    /// stream. `None` between asking for the frame and hearing how big it is.
    slot: Option<usize>,
}

/// Everything one cast holds, on the one thread it runs on.
struct Session {
    /// Held so a request sent from a PipeWire callback reaches the compositor
    /// without waiting for the next thing to arrive on the socket.
    connection: Connection,
    shm: wl_shm::WlShm,
    manager: ZwlrScreencopyManagerV1,
    output: wl_output::WlOutput,
    cursor: bool,
    queue: QueueHandle<Session>,

    /// The stream, as PipeWire's own pointer. Raw because a frame is dequeued
    /// in one callback and queued in another, and the safe wrapper hands back a
    /// buffer that queues itself when it goes out of scope — which is exactly
    /// what must not happen while the compositor is still filling it.
    stream: *mut pw::sys::pw_stream,
    slots: Vec<Slot>,
    /// What the stream is currently formatted for.
    picture: Picture,
    /// A size the display has changed to, noticed inside an event and applied
    /// once nothing is borrowing this.
    resized: Option<Picture>,
    /// Whether PipeWire has told us the format is settled. Nothing is asked of
    /// the compositor before that, because there is nowhere to put it.
    streaming: bool,
    frame: Option<InFlight>,
    /// Set when the display goes away or the stream ends, which is the only
    /// thing that stops the loop.
    finished: bool,
}

/// Run one cast until it is stopped, on this thread.
///
/// `started` is called once, as soon as PipeWire has given the stream a node
/// id: that number is what the portal hands back to the application, and until
/// it exists there is nothing to hand back.
pub fn run(
    wanted: Wanted,
    started: impl FnOnce(Live) + 'static,
    stop: pw::channel::Receiver<()>,
) -> anyhow::Result<()> {
    let connection = Connection::connect_to_env()
        .map_err(|err| anyhow::anyhow!("no LineXinBar session to record: {err}"))?;
    let (mut queue, globals) = discover(&connection, &wanted)?;

    // What size the picture is, before anything is negotiated with PipeWire:
    // one frame is asked for and thrown away, because the compositor answers
    // that question by offering a buffer and there is no other way to ask it.
    let picture = measure(&connection, &mut queue, &globals)?;
    tracing::info!(
        width = picture.width,
        height = picture.height,
        cursor = wanted.cursor,
        "sharing a display"
    );

    pw::init();
    let main_loop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&main_loop, None)?;
    let core = context.connect_rc(None)?;

    let stream = pw::stream::StreamRc::new(
        core.clone(),
        "lxb-screen",
        pw::properties::properties! {
            *pw::keys::MEDIA_CLASS => "Video/Source",
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen",
            *pw::keys::NODE_NAME => "lxb-screen",
            *pw::keys::NODE_DESCRIPTION => "LineXinBar screen",
        },
    )?;

    let session = Rc::new(RefCell::new(Session {
        connection: connection.clone(),
        shm: globals.shm.clone(),
        manager: globals.manager.clone(),
        output: globals.output.clone(),
        cursor: wanted.cursor,
        queue: queue.handle(),
        stream: stream.as_raw_ptr(),
        slots: Vec::new(),
        picture,
        resized: None,
        streaming: false,
        frame: None,
        finished: false,
    }));

    let _listener = stream
        .add_local_listener_with_user_data(session.clone())
        .state_changed(|_, session, from, to| {
            tracing::debug!(?from, ?to, "stream state");
            let mut session = session.borrow_mut();
            match to {
                pw::stream::StreamState::Error(..) => session.finished = true,
                // Somebody is watching. Ask for the first frame; from then on
                // each one asks for the next, and the screen sets the pace.
                pw::stream::StreamState::Streaming => session.ask_for_a_frame(),
                _ => {}
            }
        })
        .param_changed(|stream, session, id, param| {
            if id != pw::spa::param::ParamType::Format.as_raw() {
                return;
            }
            let Some(param) = param else {
                session.borrow_mut().streaming = false;
                return;
            };
            let mut format = VideoInfoRaw::new();
            if format.parse(param).is_err() {
                return;
            }
            let mut session = session.borrow_mut();
            let picture = session.picture;
            tracing::debug!(
                width = format.size().width,
                height = format.size().height,
                "format agreed"
            );
            // Now that the format is settled, say what buffers it needs: one
            // block of memory the size of the picture, as a memfd, because a
            // memfd is what a wl_shm pool can be made out of.
            let mut buffers = buffer_param(picture);
            let mut meta = meta_param();
            let mut params = [
                Pod::from_bytes(&buffers).unwrap(),
                Pod::from_bytes(&meta).unwrap(),
            ];
            if let Err(err) = stream.update_params(&mut params) {
                tracing::warn!(?err, "could not ask for buffers");
            }
            buffers.clear();
            meta.clear();
            session.streaming = true;
        })
        .add_buffer(|_, session, buffer| {
            let mut session = session.borrow_mut();
            if let Err(err) = session.add_slot(buffer) {
                tracing::warn!(?err, "could not lend PipeWire's buffer to the compositor");
            }
        })
        .remove_buffer(|_, session, buffer| {
            session.borrow_mut().remove_slot(buffer);
        })
        .register()?;

    let offered = format_param(picture);
    let mut params = [Pod::from_bytes(&offered).unwrap()];
    stream.connect(
        Direction::Output,
        None,
        pw::stream::StreamFlags::DRIVER | pw::stream::StreamFlags::ALLOC_BUFFERS,
        &mut params,
    )?;

    // The Wayland connection, on PipeWire's loop. One thread, two protocols,
    // and no lock between them.
    let watched = Watched {
        connection: connection.clone(),
        queue: RefCell::new(queue),
        session: session.clone(),
    };
    let _io = main_loop.loop_().add_io(
        watched,
        pw::spa::support::system::IoFlags::IN,
        |watched: &mut Watched| watched.dispatch(),
    );

    // Whatever asked for this cast can end it, from its own thread.
    let quit = main_loop.clone();
    let _stop = stop.attach(main_loop.loop_(), move |()| quit.quit());

    // The node id arrives with the stream's own state; by the time PipeWire has
    // connected it, it is there to be read.
    let announced: RefCell<Option<Announcement>> = RefCell::new(Some(Box::new(started)));
    let ticker = main_loop.clone();
    let watch = session.clone();
    let raw = stream.as_raw_ptr();
    let _timer = main_loop.loop_().add_timer(move |_| {
        if watch.borrow().finished {
            ticker.quit();
            return;
        }
        let announce = announced.borrow_mut().take();
        if let Some(announce) = announce {
            let node = unsafe { pw::sys::pw_stream_get_node_id(raw) };
            let picture = watch.borrow().picture;
            announce(Live {
                node,
                width: picture.width,
                height: picture.height,
            });
        }
    });
    _timer
        .update_timer(
            Some(std::time::Duration::from_millis(50)),
            Some(std::time::Duration::from_millis(200)),
        )
        .into_result()
        .map_err(|err| anyhow::anyhow!("could not watch the stream: {err:?}"))?;

    main_loop.run();
    tracing::info!("the cast has ended");
    Ok(())
}

/// The Wayland connection as the PipeWire loop sees it: one file descriptor,
/// and what to do when it has something to say.
struct Watched {
    connection: Connection,
    queue: RefCell<EventQueue<Session>>,
    session: Rc<RefCell<Session>>,
}

impl AsRawFd for Watched {
    fn as_raw_fd(&self) -> std::os::fd::RawFd {
        self.connection.as_fd().as_raw_fd()
    }
}

impl AsFd for Watched {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.connection.as_fd()
    }
}

impl Watched {
    fn dispatch(&mut self) {
        let mut queue = self.queue.borrow_mut();
        if let Some(guard) = self.connection.prepare_read() {
            if let Err(err) = guard.read() {
                tracing::warn!(?err, "the session hung up");
                self.session.borrow_mut().finished = true;
                return;
            }
        }
        {
            let mut session = self.session.borrow_mut();
            if let Err(err) = queue.dispatch_pending(&mut session) {
                tracing::warn!(?err, "the session hung up");
                session.finished = true;
                return;
            }
        }
        // Outside the borrow, because renegotiating with PipeWire can call
        // straight back into this session.
        let resized = self.session.borrow_mut().resized.take();
        if let Some(picture) = resized {
            self.renegotiate(picture);
        }
        let _ = self.connection.flush();
    }

    /// The display changed size under the cast. Tell PipeWire, which will agree
    /// a new format and hand back buffers of the new size.
    fn renegotiate(&self, picture: Picture) {
        tracing::info!(
            width = picture.width,
            height = picture.height,
            "the display changed size; renegotiating the stream"
        );
        let stream = self.session.borrow().stream;
        let bytes = format_param(picture);
        let params = [Pod::from_bytes(&bytes).unwrap()];
        let mut raw: Vec<*const pw::spa::sys::spa_pod> =
            params.iter().map(|p| p.as_raw_ptr() as *const _).collect();
        let result =
            unsafe { pw::sys::pw_stream_update_params(stream, raw.as_mut_ptr(), raw.len() as u32) };
        if result < 0 {
            tracing::warn!(result, "could not renegotiate the stream");
        }
        self.session.borrow_mut().picture = picture;
    }
}

impl Session {
    /// Give one of the stream's buffers a piece of memory, and give the
    /// compositor a `wl_buffer` over the same memory to copy the screen into.
    ///
    /// The memory is allocated here rather than by PipeWire, which is what the
    /// stream's `ALLOC_BUFFERS` says: a memfd this process made is a memfd it
    /// can be sure of — the right size, mapped, and the one thing a Wayland
    /// shared-memory pool can be built out of. The consumer on the other side
    /// maps the same descriptor, so the picture is written once and read where
    /// it was written.
    fn add_slot(&mut self, buffer: *mut pw::sys::pw_buffer) -> anyhow::Result<()> {
        let picture = self.picture;
        let length = picture.size();

        let file = rustix::fs::memfd_create("lxb-screen", rustix::fs::MemfdFlags::CLOEXEC)
            .map_err(|err| anyhow::anyhow!("no memory for a frame: {err}"))?;
        rustix::fs::ftruncate(&file, length as u64)
            .map_err(|err| anyhow::anyhow!("could not size a frame: {err}"))?;
        // Mapped because PipeWire hands consumers the pointer as well as the
        // descriptor, and something reading in this process would otherwise
        // have to map it a second time.
        let mapped = unsafe {
            rustix::mm::mmap(
                std::ptr::null_mut(),
                length,
                rustix::mm::ProtFlags::READ | rustix::mm::ProtFlags::WRITE,
                rustix::mm::MapFlags::SHARED,
                &file,
                0,
            )
            .map_err(|err| anyhow::anyhow!("could not map a frame: {err}"))?
        };

        let pool = self
            .shm
            .create_pool(file.as_fd(), length as i32, &self.queue, ());
        let wl_buffer = pool.create_buffer(
            0,
            picture.width as i32,
            picture.height as i32,
            picture.stride as i32,
            SHM_FORMAT,
            &self.queue,
            (),
        );

        // SAFETY: the buffer is PipeWire's, alive for as long as this slot is,
        // and every field written here is one the stream asked to be filled in.
        unsafe {
            let spa = (*buffer).buffer;
            anyhow::ensure!(
                !spa.is_null() && (*spa).n_datas > 0,
                "PipeWire offered a buffer with nothing in it"
            );
            let data = &mut *(*spa).datas;
            data.type_ = pw::spa::sys::SPA_DATA_MemFd;
            data.flags = pw::spa::sys::SPA_DATA_FLAG_READWRITE;
            data.fd = file.as_raw_fd() as i64;
            data.mapoffset = 0;
            data.maxsize = length as u32;
            data.data = mapped;
            let chunk = &mut *data.chunk;
            chunk.offset = 0;
            chunk.size = length as u32;
            chunk.stride = picture.stride as i32;
        }

        self.slots.push(Slot {
            pw: buffer,
            pool,
            buffer: wl_buffer,
            memory: mapped,
            file,
            size: length,
            stride: picture.stride,
        });
        tracing::debug!(slots = self.slots.len(), "gave PipeWire a frame's memory");
        Ok(())
    }

    fn remove_slot(&mut self, buffer: *mut pw::sys::pw_buffer) {
        // A buffer taken back while the compositor is filling it takes the
        // frame with it: what it was going to be copied into is gone.
        if self
            .frame
            .as_ref()
            .and_then(|frame| frame.slot)
            .is_some_and(|slot| self.slots.get(slot).is_some_and(|s| s.pw == buffer))
        {
            if let Some(frame) = self.frame.take() {
                frame.frame.destroy();
            }
        }
        self.slots.retain(|slot| slot.pw != buffer);
    }

    /// Ask the compositor for the next frame of the display.
    ///
    /// Asked for again the moment one is answered, which is what makes the
    /// screen the clock: `copy_with_damage` is not answered until something on
    /// that display has actually changed.
    fn ask_for_a_frame(&mut self) {
        if self.finished || !self.streaming || self.frame.is_some() {
            return;
        }
        let frame = self
            .manager
            .capture_output(self.cursor as i32, &self.output, &self.queue, ());
        self.frame = Some(InFlight { frame, slot: None });
        // Flushed here because this is called from PipeWire's side as well as
        // from Wayland's, and there the socket is not about to be written for
        // any other reason.
        let _ = self.connection.flush();
    }

    /// Take a buffer out of the stream for the compositor to fill. `None` when
    /// the consumer is holding all of them, which is a consumer that has fallen
    /// behind and is answered by skipping this frame rather than by queueing up
    /// stale ones.
    fn take_a_buffer(&mut self) -> Option<usize> {
        let buffer = unsafe { pw::sys::pw_stream_dequeue_buffer(self.stream) };
        if buffer.is_null() {
            return None;
        }
        self.slots.iter().position(|slot| slot.pw == buffer)
    }

    /// Hand a filled buffer to whoever is watching.
    fn give_back(&mut self, slot: usize, timestamp: u64) {
        let Some(entry) = self.slots.get(slot) else {
            return;
        };
        unsafe {
            let spa = (*entry.pw).buffer;
            if spa.is_null() {
                return;
            }
            let data = &mut *(*spa).datas;
            let chunk = &mut *data.chunk;
            chunk.offset = 0;
            chunk.stride = entry.stride as i32;
            chunk.size = entry.size as u32;
            chunk.flags = 0;

            // The clock the consumer stamps the frame with, when it asked for
            // one to be carried.
            let header = pw::spa::sys::spa_buffer_find_meta_data(
                spa,
                pw::spa::sys::SPA_META_Header,
                std::mem::size_of::<pw::spa::sys::spa_meta_header>(),
            ) as *mut pw::spa::sys::spa_meta_header;
            if !header.is_null() {
                (*header).pts = timestamp as i64;
                (*header).flags = 0;
                (*header).seq = 0;
                (*header).dts_offset = 0;
            }

            pw::sys::pw_stream_queue_buffer(self.stream, entry.pw);
            // A driving stream is the graph's clock, and a frame that is only
            // queued is a frame nobody has been woken up for.
            if pw::sys::pw_stream_is_driving(self.stream) {
                pw::sys::pw_stream_trigger_process(self.stream);
            }
        }
    }
}

// -- the Wayland half ------------------------------------------------------

/// The globals one cast needs.
struct Globals {
    shm: wl_shm::WlShm,
    manager: ZwlrScreencopyManagerV1,
    output: wl_output::WlOutput,
}

/// Everything found while looking for them, before one display is chosen.
#[derive(Default)]
struct Found {
    shm: Option<wl_shm::WlShm>,
    manager: Option<ZwlrScreencopyManagerV1>,
    outputs: Vec<(wl_output::WlOutput, Option<String>)>,
}

/// Block until this session ends, and answer when it has.
///
/// A portal is the session's, not the user's: it hands out that compositor's
/// screens, and when the compositor is gone there is nothing left for it to
/// hand out. Nothing else here would ever notice — every other route to Wayland
/// in this file opens a connection, uses it and drops it, so at rest the
/// process holds nothing that can fail — and a portal that does not notice is
/// one that sits on the user's bus, under the session's name, for as long as
/// they stay logged in. Five of them were found doing exactly that.
///
/// So one connection is held open for the life of the process, and read until
/// it breaks. What comes back on it is nothing: no globals are bound and no
/// events are asked for, because this is not here to do anything with the
/// session — only to be told when there is no longer one.
pub fn wait_for_the_session_to_end() -> anyhow::Result<()> {
    /// Nothing arrives, so there is nothing to keep.
    #[derive(Default)]
    struct Watching;

    impl Dispatch<wl_registry::WlRegistry, ()> for Watching {
        fn event(
            _: &mut Self,
            _: &wl_registry::WlRegistry,
            _: wl_registry::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }

    let connection = Connection::connect_to_env()
        .map_err(|err| anyhow::anyhow!("no LineXinBar session to belong to: {err}"))?;
    let mut queue = connection.new_event_queue::<Watching>();
    // A registry, so the connection carries an object and the compositor has
    // something to send on. Without one a dispatch could block for ever on a
    // connection nothing will ever write to.
    connection.display().get_registry(&queue.handle(), ());
    let mut watching = Watching;
    loop {
        if let Err(err) = queue.blocking_dispatch(&mut watching) {
            tracing::info!(%err, "the session this portal belongs to has ended");
            return Ok(());
        }
    }
}

/// The displays this session has, by the names the compositor gives them.
pub fn outputs() -> anyhow::Result<Vec<String>> {
    let connection = Connection::connect_to_env()
        .map_err(|err| anyhow::anyhow!("no LineXinBar session to look at: {err}"))?;
    let mut queue = connection.new_event_queue();
    let handle = queue.handle();
    connection.display().get_registry(&handle, ());
    let mut found = Found::default();
    queue.roundtrip(&mut found)?;
    queue.roundtrip(&mut found)?;
    Ok(found
        .outputs
        .iter()
        .map(|(output, name)| {
            name.clone()
                .unwrap_or_else(|| format!("output-{}", output.id().protocol_id()))
        })
        .collect())
}

fn discover(
    connection: &Connection,
    wanted: &Wanted,
) -> anyhow::Result<(EventQueue<Session>, Globals)> {
    let mut lookup = connection.new_event_queue::<Found>();
    let handle = lookup.handle();
    connection.display().get_registry(&handle, ());
    let mut found = Found::default();
    lookup.roundtrip(&mut found)?;
    // A second pass, because a display's name arrives after the global that
    // announced it.
    lookup.roundtrip(&mut found)?;

    let shm = found
        .shm
        .clone()
        .ok_or_else(|| anyhow::anyhow!("this compositor has no wl_shm"))?;
    let manager = found.manager.clone().ok_or_else(|| {
        anyhow::anyhow!(
            "this compositor does not offer wlr-screencopy, so its screen cannot be read"
        )
    })?;
    let output = match &wanted.output {
        Some(name) => found
            .outputs
            .iter()
            .find(|(_, seen)| seen.as_deref() == Some(name.as_str()))
            .map(|(output, _)| output.clone())
            .ok_or_else(|| anyhow::anyhow!("there is no display called {name}"))?,
        None => found
            .outputs
            .first()
            .map(|(output, _)| output.clone())
            .ok_or_else(|| anyhow::anyhow!("this session has no displays"))?,
    };

    // The objects were made on the lookup queue; everything from here runs on
    // the cast's own, which is the one the PipeWire loop dispatches.
    let queue = connection.new_event_queue::<Session>();
    Ok((
        queue,
        Globals {
            shm,
            manager,
            output,
        },
    ))
}

/// Ask for one frame purely to be told how big the display is, and throw it
/// away.
///
/// There is no other way to ask. A screencopy frame answers with the buffer it
/// wants, and that answer is the display's size, its stride and its format —
/// which is everything the stream has to be built out of.
fn measure(
    connection: &Connection,
    queue: &mut EventQueue<Session>,
    globals: &Globals,
) -> anyhow::Result<Picture> {
    let handle = queue.handle();
    let mut probe = Measure::default();
    let mut probe_queue = connection.new_event_queue::<Measure>();
    let frame = globals
        .manager
        .capture_output(0, &globals.output, &probe_queue.handle(), ());
    let _ = handle;
    for _ in 0..100 {
        probe_queue.blocking_dispatch(&mut probe)?;
        if probe.picture.is_some() || probe.failed {
            break;
        }
    }
    frame.destroy();
    let _ = connection.flush();
    probe
        .picture
        .ok_or_else(|| anyhow::anyhow!("the compositor would not say how big the display is"))
}

/// The one-shot listener behind [`measure`].
#[derive(Default)]
struct Measure {
    picture: Option<Picture>,
    failed: bool,
}

impl Dispatch<ZwlrScreencopyFrameV1, ()> for Measure {
    fn event(
        state: &mut Self,
        _frame: &ZwlrScreencopyFrameV1,
        event: zwlr_screencopy_frame_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_screencopy_frame_v1::Event::Buffer {
                format,
                width,
                height,
                stride,
            } => {
                if format == WEnum::Value(SHM_FORMAT) {
                    state.picture = Some(Picture {
                        width,
                        height,
                        stride,
                    });
                }
            }
            zwlr_screencopy_frame_v1::Event::Failed => state.failed = true,
            _ => {}
        }
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for Found {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        queue: &QueueHandle<Self>,
    ) {
        let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        else {
            return;
        };
        match interface.as_str() {
            "wl_shm" => state.shm = Some(registry.bind(name, 1, queue, ())),
            "zwlr_screencopy_manager_v1" => {
                state.manager = Some(registry.bind(name, version.min(3), queue, ()))
            }
            "wl_output" => {
                // Version 4 for the connector's name, which is how a display is
                // named everywhere else in this session.
                let output =
                    registry.bind::<wl_output::WlOutput, _, _>(name, version.min(4), queue, ());
                state.outputs.push((output, None));
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_output::WlOutput, ()> for Found {
    fn event(
        state: &mut Self,
        output: &wl_output::WlOutput,
        event: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_output::Event::Name { name } = event {
            if let Some(entry) = state.outputs.iter_mut().find(|(seen, _)| seen == output) {
                entry.1 = Some(name);
            }
        }
    }
}

macro_rules! deaf {
    ($state:ty; $($proxy:ty),* $(,)?) => {
        $(impl Dispatch<$proxy, ()> for $state {
            fn event(
                _: &mut Self,
                _: &$proxy,
                _: <$proxy as Proxy>::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
            }
        })*
    };
}

deaf!(Found; wl_shm::WlShm, ZwlrScreencopyManagerV1);
deaf!(Measure; wl_shm::WlShm, wl_output::WlOutput, ZwlrScreencopyManagerV1);
deaf!(Session; wl_shm::WlShm, wl_output::WlOutput, ZwlrScreencopyManagerV1,
      wl_shm_pool::WlShmPool, wl_buffer::WlBuffer);

impl Dispatch<ZwlrScreencopyFrameV1, ()> for Session {
    fn event(
        state: &mut Self,
        frame: &ZwlrScreencopyFrameV1,
        event: zwlr_screencopy_frame_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // Anything about a frame this session is no longer waiting on belongs
        // to a cast that has moved on.
        if state
            .frame
            .as_ref()
            .is_none_or(|in_flight| &in_flight.frame != frame)
        {
            return;
        }
        match event {
            zwlr_screencopy_frame_v1::Event::Buffer {
                format,
                width,
                height,
                stride,
            } => {
                if format != WEnum::Value(SHM_FORMAT) {
                    return;
                }
                let picture = Picture {
                    width,
                    height,
                    stride,
                };
                if picture != state.picture {
                    // The display was resized, turned or driven at another mode
                    // under the cast. Nothing here can copy that into buffers
                    // of the old size, so the frame is dropped and the stream
                    // is rebuilt around the new one.
                    state.resized = Some(picture);
                    if let Some(in_flight) = state.frame.take() {
                        in_flight.frame.destroy();
                    }
                    return;
                }
                let slot = state.take_a_buffer();
                match slot {
                    Some(slot) => {
                        let buffer = state.slots[slot].buffer.clone();
                        if let Some(in_flight) = state.frame.as_mut() {
                            in_flight.slot = Some(slot);
                        }
                        // With damage, so a screen that nobody is touching
                        // costs one frame and then nothing at all.
                        frame.copy_with_damage(&buffer);
                    }
                    None => {
                        // Every buffer is with the consumer. Drop this frame and
                        // ask again; a recorder that has fallen behind wants the
                        // newest picture, not a queue of old ones.
                        if let Some(in_flight) = state.frame.take() {
                            in_flight.frame.destroy();
                        }
                        state.ask_for_a_frame();
                    }
                }
            }
            zwlr_screencopy_frame_v1::Event::Ready {
                tv_sec_hi,
                tv_sec_lo,
                tv_nsec,
            } => {
                let seconds = ((tv_sec_hi as u64) << 32) | tv_sec_lo as u64;
                let stamp = seconds * 1_000_000_000 + tv_nsec as u64;
                let in_flight = state.frame.take();
                if let Some(slot) = in_flight.as_ref().and_then(|frame| frame.slot) {
                    state.give_back(slot, stamp);
                }
                if let Some(in_flight) = in_flight {
                    in_flight.frame.destroy();
                }
                state.ask_for_a_frame();
            }
            zwlr_screencopy_frame_v1::Event::Failed => {
                tracing::debug!("the compositor refused a frame");
                if let Some(in_flight) = state.frame.take() {
                    in_flight.frame.destroy();
                }
                // A display that has gone away fails every frame, so this is
                // also how a cast of an unplugged monitor ends rather than
                // spinning.
                state.finished = true;
            }
            _ => {}
        }
    }
}

// -- the PipeWire half -----------------------------------------------------

/// The format the stream offers: one size, one layout, take it or leave it.
///
/// There is no choice to offer. The picture is whatever the compositor is
/// drawing, in the one format screencopy hands out, and a consumer that cannot
/// take it would have to be given a converted copy — which is a thing to build
/// when something actually asks for it.
fn format_param(picture: Picture) -> Vec<u8> {
    let object = pw::spa::pod::object!(
        pw::spa::utils::SpaTypes::ObjectParamFormat,
        pw::spa::param::ParamType::EnumFormat,
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::MediaType,
            Id,
            pw::spa::param::format::MediaType::Video
        ),
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::MediaSubtype,
            Id,
            pw::spa::param::format::MediaSubtype::Raw
        ),
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::VideoFormat,
            Id,
            SPA_FORMAT
        ),
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::VideoSize,
            Rectangle,
            Rectangle {
                width: picture.width,
                height: picture.height
            }
        ),
        // Nought, which is what a source paced by something else says: the
        // frames arrive when the screen draws, and the ceiling below is what a
        // consumer sizes itself for.
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::VideoFramerate,
            Fraction,
            Fraction { num: 0, denom: 1 }
        ),
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::VideoMaxFramerate,
            Choice,
            Range,
            Fraction,
            Fraction {
                num: MAX_FRAMERATE,
                denom: 1
            },
            Fraction { num: 1, denom: 1 },
            Fraction {
                num: MAX_FRAMERATE,
                denom: 1
            }
        ),
    );
    serialise(pw::spa::pod::Value::Object(object))
}

/// What the stream needs to be given: memfds, because that is what a Wayland
/// shared-memory pool can be made out of, and one block exactly the size of the
/// picture.
fn buffer_param(picture: Picture) -> Vec<u8> {
    let object = pw::spa::pod::object!(
        pw::spa::utils::SpaTypes::ObjectParamBuffers,
        pw::spa::param::ParamType::Buffers,
        int_range(pw::spa::sys::SPA_PARAM_BUFFERS_buffers, 4, 2, 16),
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::from_raw(
                pw::spa::sys::SPA_PARAM_BUFFERS_blocks
            ),
            Int,
            1
        ),
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::from_raw(
                pw::spa::sys::SPA_PARAM_BUFFERS_size
            ),
            Int,
            picture.size() as i32
        ),
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::from_raw(
                pw::spa::sys::SPA_PARAM_BUFFERS_stride
            ),
            Int,
            picture.stride as i32
        ),
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::from_raw(
                pw::spa::sys::SPA_PARAM_BUFFERS_align
            ),
            Int,
            16
        ),
        int_flags(
            pw::spa::sys::SPA_PARAM_BUFFERS_dataType,
            1 << pw::spa::sys::SPA_DATA_MemFd,
        ),
    );
    serialise(pw::spa::pod::Value::Object(object))
}

/// One piece of metadata per frame: when it was taken, which is what a
/// recorder writes into the file it is making.
fn meta_param() -> Vec<u8> {
    let object = pw::spa::pod::object!(
        pw::spa::utils::SpaTypes::ObjectParamMeta,
        pw::spa::param::ParamType::Meta,
        pw::spa::pod::Property {
            key: pw::spa::sys::SPA_PARAM_META_type,
            flags: pw::spa::pod::PropertyFlags::empty(),
            value: pw::spa::pod::Value::Id(pw::spa::utils::Id(pw::spa::sys::SPA_META_Header)),
        },
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::from_raw(pw::spa::sys::SPA_PARAM_META_size),
            Int,
            std::mem::size_of::<pw::spa::sys::spa_meta_header>() as i32
        ),
    );
    serialise(pw::spa::pod::Value::Object(object))
}

/// One integer property with a range of acceptable values.
///
/// Written out rather than reached for with the `property!` macro: its choice
/// arms name the type through `spa::utils`, which has a `Rectangle` and a
/// `Fraction` and no plain integer.
fn int_range(key: u32, default: i32, min: i32, max: i32) -> pw::spa::pod::Property {
    pw::spa::pod::Property {
        key,
        flags: pw::spa::pod::PropertyFlags::empty(),
        value: pw::spa::pod::Value::Choice(pw::spa::pod::ChoiceValue::Int(pw::spa::utils::Choice(
            pw::spa::utils::ChoiceFlags::empty(),
            pw::spa::utils::ChoiceEnum::Range { default, min, max },
        ))),
    }
}

/// One integer property that is a set of bits rather than a number.
fn int_flags(key: u32, bits: i32) -> pw::spa::pod::Property {
    pw::spa::pod::Property {
        key,
        flags: pw::spa::pod::PropertyFlags::empty(),
        value: pw::spa::pod::Value::Choice(pw::spa::pod::ChoiceValue::Int(pw::spa::utils::Choice(
            pw::spa::utils::ChoiceFlags::empty(),
            pw::spa::utils::ChoiceEnum::Flags {
                default: bits,
                flags: vec![bits],
            },
        ))),
    }
}

fn serialise(value: pw::spa::pod::Value) -> Vec<u8> {
    pw::spa::pod::serialize::PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &value)
        .expect("a pod this crate wrote itself")
        .0
        .into_inner()
}

// -- asking the session -----------------------------------------------------

/// Put the portal's question to the session shell, and wait for the answer.
///
/// The portal cannot draw and the shell cannot see D-Bus, so the question goes
/// the one way the two of them are already connected: over `lxb_shell_v1`,
/// with the compositor carrying it to whichever shell is up and the answer
/// back. What comes back is a display or nothing, and nothing is a no —
/// including the nothing that arrives because there is no shell to ask.
///
/// Blocking, on the thread the portal answers D-Bus on, because the whole call
/// it is answering is "may this application see a screen": there is nothing to
/// get on with until somebody says.
pub fn ask_to_share(app_id: &str, patience: std::time::Duration) -> anyhow::Result<Option<String>> {
    use lxb_protocol::client::lxb_shell_v1::{self, LxbShellV1};

    /// One question, and the state of hearing it answered.
    #[derive(Default)]
    struct Asking {
        control: Option<LxbShellV1>,
        outputs: Vec<(wl_output::WlOutput, Option<String>)>,
        answered: bool,
        allowed: Option<wl_output::WlOutput>,
    }

    impl Dispatch<wl_registry::WlRegistry, ()> for Asking {
        fn event(
            state: &mut Self,
            registry: &wl_registry::WlRegistry,
            event: wl_registry::Event,
            _: &(),
            _: &Connection,
            queue: &QueueHandle<Self>,
        ) {
            let wl_registry::Event::Global {
                name,
                interface,
                version,
            } = event
            else {
                return;
            };
            match interface.as_str() {
                "lxb_shell_v1" => {
                    state.control = Some(registry.bind(name, version.min(18), queue, ()))
                }
                "wl_output" => {
                    let output =
                        registry.bind::<wl_output::WlOutput, _, _>(name, version.min(4), queue, ());
                    state.outputs.push((output, None));
                }
                _ => {}
            }
        }
    }

    impl Dispatch<wl_output::WlOutput, ()> for Asking {
        fn event(
            state: &mut Self,
            output: &wl_output::WlOutput,
            event: wl_output::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            if let wl_output::Event::Name { name } = event {
                if let Some(entry) = state.outputs.iter_mut().find(|(seen, _)| seen == output) {
                    entry.1 = Some(name);
                }
            }
        }
    }

    impl Dispatch<LxbShellV1, ()> for Asking {
        fn event(
            state: &mut Self,
            _: &LxbShellV1,
            event: lxb_shell_v1::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            // share_request is the question going *out* to the shell; it
            // reaches every other client bound to this interface, and this one
            // has no business answering its own.
            if let lxb_shell_v1::Event::ShareAnswered { output, .. } = event {
                state.answered = true;
                state.allowed = output;
            }
        }
    }

    let connection = Connection::connect_to_env()
        .map_err(|err| anyhow::anyhow!("no LineXinBar session to ask: {err}"))?;
    let mut queue = connection.new_event_queue::<Asking>();
    let handle = queue.handle();
    connection.display().get_registry(&handle, ());
    let mut asking = Asking::default();
    queue.roundtrip(&mut asking)?;
    queue.roundtrip(&mut asking)?;

    let control = asking.control.clone().ok_or_else(|| {
        anyhow::anyhow!("this compositor is not LineXinBar, so there is nobody to ask")
    })?;
    if control.version() < 18 {
        anyhow::bail!("this compositor cannot put the question to its shell");
    }
    // One question at a time from this process, so the number never has to mean
    // more than "the one being asked".
    control.ask_to_share(1, app_id.to_string());
    connection.flush()?;

    let until = std::time::Instant::now() + patience;
    while !asking.answered {
        if std::time::Instant::now() >= until {
            tracing::warn!("nobody answered the share question; taking that as a no");
            return Ok(None);
        }
        // Blocking, but bounded: the compositor refuses outright when there is
        // no shell, so the wait is the user reading the question.
        queue.blocking_dispatch(&mut asking)?;
    }

    let Some(allowed) = asking.allowed else {
        return Ok(None);
    };
    Ok(asking
        .outputs
        .iter()
        .find(|(output, _)| *output == allowed)
        .and_then(|(_, name)| name.clone()))
}
