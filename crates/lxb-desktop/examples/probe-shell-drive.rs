//! Drive a running session the way the shell does, and photograph the result.
//!
//! The shell's own protocol, spoken by something that is not the shell. It is
//! the only way to answer a question about the *live* session — which window is
//! where, what an application does when it is brought back to the front, what
//! happens to a press that lands on it — without a hand on the mouse and
//! without a nested session that is not the thing being asked about.
//!
//! Everything it can do, the shell does every day: `activate_window` is the
//! guide's card, `move_pointer` and `pointer_button` are the stick and the A
//! button aimed at a game, and `capture_window` is the compositor handing back
//! one window's own pixels — which is the only picture that can be taken of a
//! client by anybody, the shell included.
//!
//! ```text
//! probe-shell-drive list
//! probe-shell-drive activate:7 wait:1500 move:-9000,-9000 move:1280,720 click capture:7=/tmp/after.png
//! ```
//!
//! Steps run in order: `list` prints the displays and their windows,
//! `activate:<id>` brings a window to the front, `move:<dx>,<dy>` moves the
//! pointer by logical pixels (a large negative pair walks it into the top-left
//! corner of the layout, which is how an absolute position is reached through a
//! relative protocol), `click` presses and releases the left button,
//! `press`/`release` do one half each, `capture:<id>=<path>` writes a PNG of one
//! window, and `wait:<ms>` lets the session get on with it.

use std::collections::HashMap;
use std::time::Duration;

use lxb_protocol::client::lxb_shell_v1::{self, LxbShellV1};
use wayland_client::protocol::{wl_output, wl_registry};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};

/// One window as the session announced it.
#[derive(Debug, Clone, Default)]
struct Window {
    id: u32,
    title: String,
    app_id: String,
    width: u32,
    height: u32,
}

#[derive(Default)]
struct State {
    shell: Option<LxbShellV1>,
    /// Display name by `wl_output` id, for reading the list back.
    outputs: HashMap<u32, String>,
    /// And where each one starts in the layout, which is what turns the
    /// relative `move_pointer` into somewhere in particular.
    origins: HashMap<u32, (i32, i32)>,
    /// The windows on each display, replaced whole when a batch ends.
    windows: HashMap<u32, Vec<Window>>,
    pending: HashMap<u32, Vec<Window>>,
    /// The last answer to `capture_window`.
    captured: Option<(u32, String)>,
    /// And to `capture_output`, which is the whole screen rather than one
    /// window: what the user is actually looking at, chrome and all.
    shot: Option<String>,
    /// The `wl_output` proxies, to ask that of one by name.
    proxies: HashMap<u32, wl_output::WlOutput>,
}

impl State {
    fn display_of(&self, output: &wl_output::WlOutput) -> u32 {
        output.id().protocol_id()
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let steps: Vec<String> = std::env::args().skip(1).collect();
    let connection = Connection::connect_to_env()?;
    let mut queue = connection.new_event_queue();
    let handle = queue.handle();
    let display = connection.display();
    display.get_registry(&handle, ());

    let mut state = State::default();
    // Twice: the first roundtrip brings the globals, the second the events
    // that binding them produced.
    queue.roundtrip(&mut state)?;
    queue.roundtrip(&mut state)?;
    let Some(shell) = state.shell.clone() else {
        return Err("this compositor has no lxb_shell_v1".into());
    };
    // A window list arrives when the session next changes anything. Give it a
    // moment before answering questions about what is on screen.
    settle(&mut queue, &mut state, Duration::from_millis(600))?;

    for step in &steps {
        let (name, argument) = step.split_once(':').unwrap_or((step.as_str(), ""));
        match name {
            "list" => {
                for (output, windows) in &state.windows {
                    let display = state
                        .outputs
                        .get(output)
                        .cloned()
                        .unwrap_or_else(|| format!("wl_output@{output}"));
                    let origin = state.origins.get(output).copied().unwrap_or((0, 0));
                    println!("display {display} at {},{}", origin.0, origin.1);
                    for window in windows {
                        println!(
                            "  id={} {}x{} app_id={:?} title={:?}",
                            window.id, window.width, window.height, window.app_id, window.title
                        );
                    }
                }
            }
            "activate" => {
                let id: u32 = argument.parse()?;
                println!("activate {id}");
                shell.activate_window(id);
            }
            "move" => {
                let (dx, dy) = argument.split_once(',').ok_or("move:<dx>,<dy>")?;
                let (dx, dy): (f64, f64) = (dx.parse()?, dy.parse()?);
                println!("move {dx},{dy}");
                shell.move_pointer(dx, dy);
            }
            "click" | "press" | "release" => {
                const LEFT: u32 = 0x110;
                if name != "release" {
                    println!("press");
                    shell.pointer_button(LEFT, lxb_shell_v1::ButtonState::Pressed);
                }
                if name == "click" {
                    connection.flush()?;
                    std::thread::sleep(Duration::from_millis(60));
                }
                if name != "press" {
                    println!("release");
                    shell.pointer_button(LEFT, lxb_shell_v1::ButtonState::Released);
                }
            }
            "shoot" => {
                let (display, path) = argument.split_once('=').ok_or("shoot:<display>=<path>")?;
                let output = state
                    .outputs
                    .iter()
                    .find(|(_, name)| name.as_str() == display)
                    .and_then(|(id, _)| state.proxies.get(id))
                    .cloned()
                    .ok_or_else(|| format!("no display called {display:?}"))?;
                state.shot = None;
                shell.capture_output(&output, path.to_string());
                connection.flush()?;
                settle(&mut queue, &mut state, Duration::from_millis(3000))?;
                match &state.shot {
                    Some(path) if !path.is_empty() => println!("photographed {path}"),
                    _ => println!("capture of {display} wrote nothing"),
                }
            }
            "capture" => {
                let (id, path) = argument.split_once('=').ok_or("capture:<id>=<path>")?;
                let id: u32 = id.parse()?;
                state.captured = None;
                shell.capture_window(id, path.to_string());
                connection.flush()?;
                settle(&mut queue, &mut state, Duration::from_millis(3000))?;
                match &state.captured {
                    Some((_, path)) if !path.is_empty() => println!("captured {path}"),
                    _ => println!("capture of {id} wrote nothing"),
                }
            }
            "wait" => {
                let ms: u64 = argument.parse()?;
                connection.flush()?;
                settle(&mut queue, &mut state, Duration::from_millis(ms))?;
            }
            other => return Err(format!("unknown step {other:?}").into()),
        }
        connection.flush()?;
    }
    settle(&mut queue, &mut state, Duration::from_millis(300))?;
    Ok(())
}

/// Read events for a while, so the session has said everything it is going to.
fn settle(
    queue: &mut wayland_client::EventQueue<State>,
    state: &mut State,
    how_long: Duration,
) -> Result<(), Box<dyn std::error::Error>> {
    let until = std::time::Instant::now() + how_long;
    while std::time::Instant::now() < until {
        queue.dispatch_pending(state)?;
        queue.flush()?;
        std::thread::sleep(Duration::from_millis(50));
        queue.prepare_read().map(|guard| guard.read().ok());
    }
    queue.dispatch_pending(state)?;
    Ok(())
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        handle: &QueueHandle<Self>,
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
            "wl_output" => {
                registry.bind::<wl_output::WlOutput, _, _>(name, version.min(4), handle, ());
            }
            "lxb_shell_v1" => {
                state.shell =
                    Some(registry.bind::<LxbShellV1, _, _>(name, version.min(25), handle, ()));
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_output::WlOutput, ()> for State {
    fn event(
        state: &mut Self,
        output: &wl_output::WlOutput,
        event: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_output::Event::Name { name } => {
                state.outputs.insert(output.id().protocol_id(), name);
            }
            wl_output::Event::Geometry { x, y, .. } => {
                state.origins.insert(output.id().protocol_id(), (x, y));
                state
                    .proxies
                    .insert(output.id().protocol_id(), output.clone());
            }
            _ => {}
        }
    }
}

impl Dispatch<LxbShellV1, ()> for State {
    fn event(
        state: &mut Self,
        _shell: &LxbShellV1,
        event: lxb_shell_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            lxb_shell_v1::Event::OutputWindow {
                output,
                id,
                title,
                width,
                height,
            } => {
                let display = state.display_of(&output);
                state.pending.entry(display).or_default().push(Window {
                    id,
                    title,
                    width,
                    height,
                    ..Default::default()
                });
            }
            lxb_shell_v1::Event::OutputWindowAppId { output, id, app_id } => {
                let display = state.display_of(&output);
                if let Some(window) = state
                    .pending
                    .entry(display)
                    .or_default()
                    .iter_mut()
                    .find(|window| window.id == id)
                {
                    window.app_id = app_id;
                }
            }
            lxb_shell_v1::Event::OutputWindowsDone { output } => {
                let display = state.display_of(&output);
                let windows = state.pending.remove(&display).unwrap_or_default();
                state.windows.insert(display, windows);
            }
            lxb_shell_v1::Event::WindowCaptured { id, path } => {
                state.captured = Some((id, path));
            }
            lxb_shell_v1::Event::OutputCaptured { path, .. } => {
                state.shot = Some(path);
            }
            _ => {}
        }
    }
}
