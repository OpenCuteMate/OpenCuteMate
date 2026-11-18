//! Virtual-pointer input injector (test/debug tool).
//!
//! Connects to a Wayland compositor that implements
//! `zwlr_virtual_pointer_manager_v1` (Niri, wlroots-based compositors) and
//! injects pointer events **without touching any physical device** — the
//! perfect input source for automated tests: deterministic absolute
//! coordinates, no pointer acceleration, no host interference.
//!
//! The injector reads simple line commands from stdin (so one connection,
//! one virtual pointer, can drive a whole scripted sequence):
//!
//! ```text
//! move <x> <y>                 absolute move, extents default to 1000x1000
//! move <x> <y> <x_extent> <y_extent>   absolute move with explicit extents
//! down <button>                press   (button = Linux code: 0x110 left,
//! up <button>                  release   0x111 right, 0x112 middle)
//! click <button>               press + release
//! quit                         exit
//! ```
//!
//! Absolute coordinates are compositor-wide fractions: the compositor maps
//! `(x / x_extent, y / y_extent)` onto its output layout. With a single
//! output of logical size W×H, `move X Y W H` lands the pointer exactly at
//! logical (X, Y).
//!
//! usage:
//! ```sh
//! WAYLAND_DISPLAY=<socket> cargo run -p ocm-platform-wayland --example inject
//! ```

use std::io::{BufRead, Write};

use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{wl_pointer::ButtonState, wl_registry::WlRegistry, wl_seat::WlSeat};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols_wlr::virtual_pointer::v1::client::{
    zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1,
    zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
};

#[derive(Default)]
struct State;

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(
        _state: &mut Self,
        _proxy: &WlRegistry,
        _event: <WlRegistry as Proxy>::Event,
        _data: &GlobalListContents,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwlrVirtualPointerManagerV1, ()> for State {
    fn event(
        _state: &mut Self,
        _proxy: &ZwlrVirtualPointerManagerV1,
        _event: <ZwlrVirtualPointerManagerV1 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwlrVirtualPointerV1, ()> for State {
    fn event(
        _state: &mut Self,
        _proxy: &ZwlrVirtualPointerV1,
        _event: <ZwlrVirtualPointerV1 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        // zwlr_virtual_pointer_v1 has no events.
    }
}

const BTN_LEFT: u32 = 0x110;

/// Parse a number that may be decimal ("272") or hex ("0x110").
fn parse_u32(s: &str) -> Option<u32> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u32::from_str_radix(hex, 16).ok()
    } else {
        s.parse::<u32>().ok()
    }
}

fn main() {
    let conn = Connection::connect_to_env().expect("connect to Wayland (set WAYLAND_DISPLAY)");
    let (globals, queue) = registry_queue_init::<State>(&conn).expect("registry roundtrip");
    let qh = queue.handle();

    let manager: ZwlrVirtualPointerManagerV1 = globals
        .bind(&qh, 1..=2, ())
        .expect("compositor does not support zwlr_virtual_pointer_manager_v1");
    let pointer = manager.create_virtual_pointer(None::<&WlSeat>, &qh, ());

    let stdin = std::io::stdin();
    let mut time: u32 = 0;
    let mut stdout = std::io::stdout();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let mut parts = line.split_whitespace();
        let cmd = match parts.next() {
            Some(c) => c,
            None => continue,
        };
        let nums: Vec<u32> = parts.map(|p| parse_u32(p).unwrap_or(u32::MAX)).collect();

        let handled = match (cmd, nums.as_slice()) {
            ("move", [x, y]) => {
                pointer.motion_absolute(time, *x, *y, 1000, 1000);
                pointer.frame();
                true
            }
            ("move", [x, y, xe, ye]) => {
                pointer.motion_absolute(time, *x, *y, *xe, *ye);
                pointer.frame();
                true
            }
            ("down", [button]) => {
                pointer.button(time, *button, ButtonState::Pressed);
                pointer.frame();
                true
            }
            ("up", [button]) => {
                pointer.button(time, *button, ButtonState::Released);
                pointer.frame();
                true
            }
            ("click", [button]) => {
                pointer.button(time, *button, ButtonState::Pressed);
                pointer.frame();
                pointer.button(time + 5, *button, ButtonState::Released);
                pointer.frame();
                true
            }
            ("click", []) => {
                pointer.button(time, BTN_LEFT, ButtonState::Pressed);
                pointer.frame();
                pointer.button(time + 5, BTN_LEFT, ButtonState::Released);
                pointer.frame();
                true
            }
            ("quit", []) => break,
            _ => {
                eprintln!("unknown command: {line:?}");
                false
            }
        };

        if handled {
            // Acknowledge so the driving script can synchronize if it wants.
            let _ = writeln!(stdout, "ok {cmd}");
            let _ = stdout.flush();
            time = time.wrapping_add(10);
        }
        let _ = conn.flush();
    }

    pointer.destroy();
    let _ = conn.flush();
}
