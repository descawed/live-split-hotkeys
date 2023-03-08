use std::borrow::Cow;
use std::collections::HashMap;

use anyhow::{anyhow, Result};
use clap::Parser;
use quick_xml::Reader;
use x11rb::{atom_manager, connect};
use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::Event;
use x11rb::protocol::xkb::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{Atom, AtomEnum, ConnectionExt as _, GrabMode, ModMask};
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
    /// X window ID of the LiveSplit window
    #[arg(short, long)]
    window_id: Option<u32>,
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
            None => self.find_window(screen.root)?,
        };
        println!("Window found: {}", window);

        // start listening for hotkeys
        self.conn.grab_key(true, screen.root, ModMask::ANY, GRAB_ANY, GrabMode::ASYNC, GrabMode::ASYNC)?.check()?;
        loop {
            match self.conn.wait_for_event()? {
                Event::XkbStateNotify(event) => {
                    if event.device_id as i32 == device_id {
                        state.update_mask(event.base_mods.into(), event.latched_mods.into(), event.locked_mods.into(),
                            event.base_group.try_into().unwrap(), event.latched_group.try_into().unwrap(), event.locked_group.into());
                    }
                }
                Event::KeyPress(event) | Event::KeyRelease(event) => {
                    println!("{}", state.key_get_utf8(event.detail.into()));
                }
                _ => ()
            }
        }

        Ok(())
    }

    fn window_name_matches(&self, window: u32, target_name: &str) -> Result<bool> {
        // atoms and order taken from xdotool
        let atoms: [Atom; 4] = [self.atoms._NET_WM_NAME.into(), AtomEnum::WM_NAME.into(), AtomEnum::STRING.into(), self.atoms.UTF8_STRING.into()];
        for atom in atoms {
            let result = self.conn.get_property(false, window, atom, AtomEnum::STRING, 0, u32::MAX)?;
            let reply = result.reply()?;
            let name = String::from_utf8_lossy(&reply.value);
            if name == target_name {
                return Ok(true);
            }
        }

        Ok(false)
    }

    fn find_window(&self, root_window: u32) -> Result<u32> {
        let query = self.conn.query_tree(root_window)?;
        let tree = query.reply()?;
        let windows: Vec<_> = tree.children.iter().copied().filter(|c| self.window_name_matches(*c, "LiveSplit").unwrap_or(false)).collect();

        match windows.len() {
            0 => Err(anyhow!("LiveSplit window not found")),
            1 => Ok(windows[0]),
            _ => Err(anyhow!("Multiple candidate windows found: {:?}", windows)),
        }
    }
}

fn main() -> Result<()> {
    let listener = HotkeyListener::new(Args::parse())?;
    listener.listen()
}
