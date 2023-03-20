use std::mem::size_of;
use std::ptr::addr_of;
use std::time::Duration;

use anyhow::{anyhow, Result};
use async_std::channel::{unbounded, Receiver, Sender};
use async_std::fs::{read_dir, File};
use async_std::io::ReadExt;
use async_std::path::PathBuf;
use async_std::prelude::StreamExt;
use async_std::task;
use clap::Parser;
use futures::future;
use input_event_codes_hashmap::EV;
use libc::input_event;
use procfs::process;
use x11rb::atom_manager;
use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::xkb::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ConnectionExt as _, EventMask, KeyButMask, KeyPressEvent,
    Keycode as X11rbKeycode, Window, KEY_PRESS_EVENT, KEY_RELEASE_EVENT,
};
use x11rb::xcb_ffi::XCBConnection;

mod key;
use key::*;

atom_manager! {
    pub AtomCollection: AtomCollectionCookie {
        _NET_WM_NAME,
        _NET_WM_PID,
        UTF8_STRING,
    }
}

static EVENT_SEQUENCE: [(u8, EventMask, bool); 3] = [
    (KEY_RELEASE_EVENT, EventMask::KEY_RELEASE, true),
    (KEY_PRESS_EVENT, EventMask::KEY_PRESS, true),
    (KEY_RELEASE_EVENT, EventMask::KEY_RELEASE, false),
];

/// Listen for LiveSplit hotkeys
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Path to LiveSplit's settings.cfg where the hotkeys will be read from
    #[arg(short, long)]
    settings: Option<String>,
    /// PID of the LiveSplit process to help identify the correct window
    #[arg(short, long)]
    pid: Option<i32>,
    /// X window ID of the LiveSplit window
    #[arg(short, long)]
    window: Option<u32>,
    /// Path to the keyboard device file(s) to read from
    #[arg(short, long)]
    devices: Vec<String>,
    /// How long to wait, in milliseconds, between pressing and releasing keys
    #[arg(short, long, default_value_t = 50)]
    key_delay: u64,
    /// Display debug information. Specify twice to show every key event.
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,
}

#[derive(Debug, PartialEq)]
enum WindowMatch {
    Full(Window),
    Name(Window),
    None,
}

impl WindowMatch {
    pub fn best(self, other: Self) -> Self {
        match other {
            Self::Full(_) => other,
            Self::None => self,
            _ => {
                if self == Self::None {
                    other
                } else {
                    self
                }
            }
        }
    }
}

impl Into<Option<Window>> for WindowMatch {
    fn into(self) -> Option<Window> {
        match self {
            Self::Full(w) | Self::Name(w) => Some(w),
            _ => None,
        }
    }
}

struct HotkeyListener {
    args: Args,
    _xcb_conn: xcb::Connection, // need to keep this alive even though we don't use it again because conn needs it
    screen_num: usize,
    conn: XCBConnection,
    atoms: AtomCollection,
    key_state: KeyState,
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
            return Err(anyhow!("xkb extension not supported"));
        }

        let key_state = KeyState::new(&xcb_conn, args.settings.as_deref())?;
        Ok(Self {
            args,
            _xcb_conn: xcb_conn,
            screen_num,
            conn,
            atoms,
            key_state,
        })
    }

    async fn listen_keyboard(sender: Sender<(u32, bool)>, path: PathBuf) -> Result<()> {
        let ev_key = EV["KEY"] as u16;
        let mut file = File::open(path).await?;
        loop {
            let (type_, code, value) = {
                let mut event_buf = [0u8; size_of::<input_event>()];
                file.read_exact(&mut event_buf).await?;
                // I don't think this is that bad because an input_event is ultimately all ints, so there are no invalid
                // bit patterns, and binrw would just be reading the exact same bytes in the exact same sequence.
                let event = unsafe { &*(addr_of!(event_buf) as *const input_event) };
                (event.type_, event.code, event.value)
            };
            // 2 = autorepeat, which we don't want to listen for
            if type_ == ev_key && value < 2 {
                let raw_code = code as u32;
                sender.send((raw_code, value != 0)).await?;
            }
        }
    }

    async fn listen_keys(
        mut self,
        receiver: Receiver<(u32, bool)>,
        window: u32,
        root: Window,
    ) -> Result<()> {
        let key_delay = Duration::from_millis(self.args.key_delay);

        loop {
            let (code, is_pressed) = receiver.recv().await?;
            if self.args.verbose > 1 {
                println!("Key {} = {}", code, is_pressed);
            }
            let active_hotkeys = self.key_state.handle_key(code, is_pressed);
            if !active_hotkeys.is_empty() {
                let focused_window = self.conn.get_input_focus()?.reply()?.focus;
                if focused_window == window {
                    if self.args.verbose > 0 {
                        println!("Not sending hotkey because LiveSplit already has focus");
                    }
                    continue;
                }
            }

            for keys_to_send in active_hotkeys {
                if self.args.verbose > 0 {
                    println!("Sending hotkey {:?}", keys_to_send);
                }
                let mut event_to_send = KeyPressEvent {
                    response_type: KEY_PRESS_EVENT,
                    detail: 0,
                    sequence: 0,
                    time: x11rb::CURRENT_TIME,
                    root,
                    event: window,
                    child: x11rb::NONE,
                    root_x: 1,
                    root_y: 1,
                    event_x: 1,
                    event_y: 1,
                    state: KeyButMask::CONTROL,
                    same_screen: true,
                };

                for (response_type, mask, do_sleep) in EVENT_SEQUENCE.iter().copied() {
                    for key in &keys_to_send {
                        event_to_send.response_type = response_type;
                        event_to_send.detail = *key as X11rbKeycode;
                        self.conn
                            .send_event(true, window, mask, event_to_send)?
                            .check()?;
                    }
                    if do_sleep {
                        task::sleep(key_delay).await;
                    }
                }
            }
        }
    }

    pub async fn listen(self) -> Result<()> {
        let root = self.conn.setup().roots[self.screen_num].root;
        let window = match self.args.window {
            Some(id) => id,
            None => self
                .find_window(root)?
                .ok_or_else(|| anyhow!("LiveSplit window not found"))?,
        };
        if self.args.verbose > 0 {
            println!("Window found: {}", window);
        }

        // find keyboards
        let devices = if !self.args.devices.is_empty() {
            self.args.devices.iter().map(PathBuf::from).collect()
        } else {
            let mut devices = Vec::new();
            let mut entries = read_dir("/dev/input/by-path/").await?;
            while let Some(entry) = entries.next().await {
                let path = entry?.path();
                if path
                    .file_name()
                    .map_or(false, |n| n.to_string_lossy().ends_with("-event-kbd"))
                {
                    devices.push(path);
                }
            }
            devices
        };

        if devices.is_empty() {
            return Err(anyhow!("No keyboard devices found"));
        }

        if self.args.verbose > 0 {
            println!("Keyboards: {:?}", devices);
        }
        let (sender, receiver) = unbounded();
        let mut tasks: Vec<_> = devices
            .into_iter()
            .map(|d| task::spawn(Self::listen_keyboard(sender.clone(), d)))
            .collect();
        tasks.push(task::spawn(self.listen_keys(receiver, window, root)));
        future::try_join_all(tasks).await.map(|_| ())
    }

    fn window_matches(
        &self,
        window: Window,
        target_name: &str,
        pid: Option<i32>,
    ) -> Result<WindowMatch> {
        // atoms and order taken from xdotool
        let atoms: [Atom; 4] = [
            self.atoms._NET_WM_NAME,
            AtomEnum::WM_NAME.into(),
            AtomEnum::STRING.into(),
            self.atoms.UTF8_STRING,
        ];
        for atom in atoms {
            let result =
                self.conn
                    .get_property(false, window, atom, AtomEnum::STRING, 0, u32::MAX)?;
            let reply = result.reply()?;
            let name = String::from_utf8_lossy(&reply.value);
            if name == target_name {
                if let Some(expected_pid) = pid {
                    let result = self.conn.get_property(
                        false,
                        window,
                        self.atoms._NET_WM_PID,
                        AtomEnum::ANY,
                        0,
                        u32::MAX,
                    )?;
                    let reply = result.reply()?;
                    if let Some(actual_pid) = reply.value32().and_then(|i| i.last()) {
                        // per xdotool: /* The data itself is unsigned long, but everyone uses int as pid values */
                        let actual_pid = actual_pid as i32;
                        if actual_pid == expected_pid {
                            return Ok(WindowMatch::Full(window));
                        }
                    }
                }
                return Ok(WindowMatch::Name(window));
            }
        }

        Ok(WindowMatch::None)
    }

    fn find_window(&self, root_window: u32) -> Result<Option<Window>> {
        // first, try to find the PID of the LiveSplit process
        let mut pid = self.args.pid;
        if pid.is_none() {
            for proc in process::all_processes()? {
                let proc = proc?;
                if proc
                    .cmdline()?
                    .last()
                    .map_or(false, |s| s.ends_with("\\LiveSplit.exe"))
                {
                    pid = Some(proc.pid);
                    break;
                }
            }
        }

        let query = self.conn.query_tree(root_window)?;
        let tree = query.reply()?;
        Ok(tree
            .children
            .iter()
            .copied()
            .map(|w| self.window_matches(w, "LiveSplit", pid))
            .reduce(|acc, result| {
                acc.and_then(|acc_match| result.map(|result_match| acc_match.best(result_match)))
            })
            .unwrap_or(Ok(WindowMatch::None))?
            .into())
    }
}

#[async_std::main]
async fn main() -> Result<()> {
    let listener = HotkeyListener::new(Args::parse())?;
    listener.listen().await
}
