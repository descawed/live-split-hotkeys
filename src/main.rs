use std::borrow::Cow;
use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::mem::size_of;
use std::ptr::addr_of;

use anyhow::{anyhow, Result};
use clap::Parser;
use input_event_codes_hashmap::{EV, KEY};
use libc::{c_ulong, input_event, timeval};
use procfs::process;
use quick_xml::Reader;
use x11rb::{atom_manager, connect};
use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::Event;
use x11rb::protocol::xkb::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{Atom, AtomEnum, ConnectionExt as _, EventMask, GrabMode, KEY_PRESS_EVENT, KEY_RELEASE_EVENT, KeyButMask, KeyPressEvent, ModMask, Window};
use x11rb::xcb_ffi::XCBConnection;
use xcb::x::GRAB_ANY;
use xkbcommon::xkb as xkbc;
use xkbcommon::xkb::{Keycode, Keysym};

atom_manager! {
    pub AtomCollection: AtomCollectionCookie {
        _NET_WM_NAME,
        _NET_WM_PID,
        UTF8_STRING,
    }
}

/// Listen for LiveSplit hotkeys
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Path to LiveSplit's settings.cfg where the hotkeys will be read from
    #[arg(short, long)]
    settings_path: Option<String>,
    /// PID of the LiveSplit process to help identify the correct window
    #[args(short, long)]
    pid: Option<i32>,
    /// X window ID of the LiveSplit window
    #[arg(short, long)]
    window_id: Option<u32>,
}

#[derive(PartialEq)]
enum WindowMatch {
    FullMatch(Window),
    NameMatch(Window),
    NoMatch,
}

impl WindowMatch {
    pub fn best(self, other: Self) -> Self {
        match other {
            Self::FullMatch(_) => other,
            Self::NoMatch => self,
            _ => if self == Self::NoMatch { other } else { self },
        }
    }
}

impl Into<Option<Window>> for WindowMatch {
    fn into(self) -> Option<Window> {
        match self {
            Self::FullMatch(w) | Self::NameMatch(w) => Some(w),
            _ => None,
        }
    }
}

struct HotkeyListener {
    args: Args,
    xcb_conn: xcb::Connection,
    screen_num: usize,
    conn: XCBConnection,
    atoms: AtomCollection,
}

impl HotkeyListener {
    pub fn new(args: Args) -> Result<Self> {
        // taken from https://github.com/psychon/x11rb/issues/782
        // The XCB crate requires ownership of the connection, so we need to use it to connect to the
        // X11 server.
        let (xcb_conn, screen_num) = xcb::Connection::connect(None)?;
        let screen_num = usize::try_from(screen_num)?;
        // Now get us an x11rb connection using the same underlying libxcb connection
        let conn = {
            let raw_conn = xcb_conn.get_raw_conn().cast();
            unsafe { XCBConnection::from_raw_xcb_connection(raw_conn, false) }
        }?;

        // intern atoms and enable xkb extension
        conn.prefetch_extension_information(xkb::X11_EXTENSION_NAME)?;
        let atoms = AtomCollection::new(&conn)?;
        let xkb = conn.xkb_use_extension(1, 0)?;
        let atoms = atoms.reply()?;
        let xkb = xkb.reply()?;
        if !xkb.supported {
            Err(anyhow!("xkb extension not supported"))
        } else {
            Ok(HotkeyListener {
                args,
                xcb_conn,
                screen_num,
                conn,
                atoms,
            })
        }
    }

    pub fn listen(self) -> Result<()> {
        // get keyboard state. documentation: https://xkbcommon.org/doc/current/group__x11.html
        let context = xkbc::Context::new(xkbc::CONTEXT_NO_FLAGS);
        let device_id = xkbc::x11::get_core_keyboard_device_id(&self.xcb_conn);
        let keymap = xkbc::x11::keymap_new_from_device(
            &context,
            &self.xcb_conn,
            device_id,
            xkbc::KEYMAP_COMPILE_NO_FLAGS,
        );
        let mut state = xkbc::x11::state_new_from_device(&keymap, &self.xcb_conn, device_id);

        // we're going to watch for the individual keys listed in the LiveSplit config, so let's make a
        // map to look up which key code corresponds to each key name
        let sym_to_code: HashMap<Keysym, Keycode> = (keymap.min_keycode()..=keymap.max_keycode())
            .flat_map(|c| state.key_get_syms(c).into_iter().filter_map(move |s| {
                match *s {
                    xkbc::keysyms::KEY_NoSymbol | xkbc::keysyms::KEY_VoidSymbol => None,
                    _ => Some((*s, c)),
                }
            })).collect();

        let screen = &self.conn.setup().roots[self.screen_num];
        let window = match self.args.window_id {
            Some(id) => id,
            None => self.find_window(screen.root)?.ok_or_else(|| anyhow!("LiveSplit window not found"))?,
        };
        println!("Window found: {}", window);

        let ev_key = EV["KEY"] as u16;
        let mut event_buf = [0u8; size_of::<input_event>()];
        // TODO: look up keyboard device path
        let mut file = File::open("/dev/input/by-path/pci-0000:00:14.0-usb-0:5.2.4:1.0-event-kbd")?;
        loop {
            file.read_exact(&mut event_buf)?;
            // I don't think this is that bad because an input_event is ultimately all ints, so there are no invalid
            // bit patterns, and binrw would just be reading the exact same bytes in the exact same sequence.
            let event = unsafe { &*(addr_of!(event_buf) as *const input_event) };
            if event.type_ == ev_key {
                let event_to_send = KeyPressEvent {
                    response_type: if event.value == 0 { KEY_RELEASE_EVENT } else { KEY_PRESS_EVENT },
                    detail: 42,
                    sequence: 0,
                    time: x11rb::CURRENT_TIME,
                    root: screen.root,
                    event: window,
                    child: x11rb::NONE,
                    root_x: 1,
                    root_y: 1,
                    event_x: 1,
                    event_y: 1,
                    state: KeyButMask::CONTROL,
                    same_screen: true,
                };
                self.conn.send_event(true, window, if event.value == 0 { EventMask::KEY_RELEASE } else { EventMask::KEY_PRESS }, event_to_send)?.check()?;
                println!("{:#x} was {}", event.code, if event.value == 0 { "released" } else { "pressed" });
            }
        }
    }

    fn window_matches(&self, window: Window, target_name: &str, pid: Option<i32>) -> Result<WindowMatch> {
        // atoms and order taken from xdotool
        let atoms: [Atom; 4] = [self.atoms._NET_WM_NAME.into(), AtomEnum::WM_NAME.into(), AtomEnum::STRING.into(), self.atoms.UTF8_STRING.into()];
        for atom in atoms {
            let result = self.conn.get_property(false, window, atom, AtomEnum::STRING, 0, u32::MAX)?;
            let reply = result.reply()?;
            let name = String::from_utf8_lossy(&reply.value);
            if name == target_name {
                if let Some(expected_pid) = pid {
                    let result = self.conn.get_property(false, window, self.atoms._NET_WM_PID, AtomEnum::ANY, 0, u32::MAX)?;
                    let reply = result.reply()?;
                    if let Some(actual_pid) = reply.value32().and_then(|i| i.last()) {
                        // per xdotool: /* The data itself is unsigned long, but everyone uses int as pid values */
                        let actual_pid = actual_pid as i32;
                        if actual_pid == expected_pid {
                            return Ok(WindowMatch::FullMatch(window));
                        }
                    }
                }
                return Ok(WindowMatch::NameMatch(window));
            }
        }

        Ok(WindowMatch::NoMatch)
    }

    fn find_window(&self, root_window: u32) -> Result<Option<Window>> {
        // first, try to find the PID of the LiveSplit process
        let mut pid = self.args.pid;
        if pid.is_none() {
            for proc in process::all_processes()? {
                let proc = proc?;
                if proc.cmdline()?.last().map_or(false, |s| s.ends_with("\\LiveSplit.exe")) {
                    pid = Some(proc.pid);
                    break;
                }
            }
        }

        let query = self.conn.query_tree(root_window)?;
        let tree = query.reply()?;
        Ok(tree.children.iter().copied()
            .map(|w| self.window_matches(w, "LiveSplit", pid))
            .reduce(|acc, result| acc.and_then(|acc_match| result.map(|result_match| acc_match.best(result_match))))
            .unwrap_or(Ok(WindowMatch::NoMatch))?.into())
    }
}

fn main() -> Result<()> {
    let listener = HotkeyListener::new(Args::parse())?;
    listener.listen()
}
