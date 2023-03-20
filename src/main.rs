use std::collections::HashMap;
use std::env;
use std::mem::size_of;
use std::ptr::addr_of;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use async_std::channel::{Sender, Receiver, unbounded};
use async_std::fs::{File, read_dir};
use async_std::io::{Read, ReadExt};
use async_std::path::PathBuf;
use async_std::prelude::{FutureExt, StreamExt};
use async_std::task;
use clap::Parser;
use futures::future;
use input_event_codes_hashmap::{EV, KEY};
use libc::input_event;
use procfs::process;
use quick_xml::Reader as XmlReader;
use quick_xml::events::Event as XmlEvent;
use x11rb::{atom_manager, connect};
use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::Event;
use x11rb::protocol::xkb::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{Atom, AtomEnum, ConnectionExt as _, EventMask, GrabMode, KEY_PRESS_EVENT, KEY_RELEASE_EVENT, Keycode as X11rbKeycode, KeyButMask, KeyPressEvent, ModMask, Window};
use x11rb::xcb_ffi::XCBConnection;
use xkbcommon::xkb as xkbc;
use xkbcommon::xkb::{Keycode, Keysym};

atom_manager! {
    pub AtomCollection: AtomCollectionCookie {
        _NET_WM_NAME,
        _NET_WM_PID,
        UTF8_STRING,
    }
}

const NUM_HOTKEYS: usize = 8;
static EVENT_SEQUENCE: [(u8, EventMask, bool); 3] = [
    (KEY_RELEASE_EVENT, EventMask::KEY_RELEASE, true),
    (KEY_PRESS_EVENT, EventMask::KEY_PRESS, true),
    (KEY_RELEASE_EVENT, EventMask::KEY_RELEASE, false)
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
    #[arg(short, long, default_value_t=50)]
    key_delay: u64,
}

#[derive(Debug, PartialEq)]
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

#[derive(Debug)]
struct Keymapper {
    key_map: HashMap<&'static str, &'static str>,
}

impl Keymapper {
    pub fn new() -> Self {
        let key_map = HashMap::from([
            ("Control", "LEFTCTRL"),
            ("ControlKey", "LEFTCTRL"),
            ("LControlKey", "LEFTCTRL"),
            ("Alt", "LEFTALT"),
            ("LMenu", "LEFTALT"),
            ("Back", "BACKSPACE"),
            ("Capital", "CAPSLOCK"),
            ("Escape", "ESC"),
            ("LShiftKey", "LEFTSHIFT"),
            ("Shift", "LEFTSHIFT"),
            ("ShiftKey", "LEFTSHIFT"),
            ("LWin", "LEFTMETA"),
            ("Next", "PAGEDOWN"),
            ("Prior", "PAGEUP"),
            ("D0", "0"),
            ("D1", "1"),
            ("D2", "2"),
            ("D3", "3"),
            ("D4", "4"),
            ("D5", "5"),
            ("D6", "6"),
            ("D7", "7"),
            ("D8", "8"),
            ("D9", "9"),
            ("NumPad0", "KP0"),
            ("NumPad1", "KP1"),
            ("NumPad2", "KP2"),
            ("NumPad3", "KP3"),
            ("NumPad4", "KP4"),
            ("NumPad5", "KP5"),
            ("NumPad6", "KP6"),
            ("NumPad7", "KP7"),
            ("NumPad8", "KP8"),
            ("NumPad9", "KP9"),
            ("OemBackslash", "BACKSLASH"),
            ("OemClear", "CLEAR"),
            ("OemCloseBrackets", "RIGHTBRACE"),
            ("Oemcomma", "COMMA"),
            ("OemMinus", "MINUS"),
            ("OemOpenBrackets", "LEFTBRACE"),
            ("OemPeriod", "DOT"),
            ("OemPipe", "BACKSLASH"),
            ("Oemplus", "EQUAL"),
            ("OemQuestion", "SLASH"),
            ("OemQuotes", "APOSTROPHE"),
            ("OemSemicolon", "SEMICOLON"),
            ("Oemtilde", "GRAVE"),
            ("RControlKey", "RIGHTCTRL"),
            ("Return", "ENTER"),
            ("RShiftKey", "RIGHTSHIFT"),
            ("RWin", "RIGHTMETA"),
            ("Scroll", "SCROLLLOCK"),
        ]);

        Keymapper { key_map }
    }

    pub fn map(&self, name: &str) -> Option<u32> {
        if let Some(mapped_name) = self.key_map.get(name).copied() {
            KEY.get(mapped_name).copied()
        } else {
            let uc_name = name.to_uppercase();
            KEY.get(uc_name.as_str()).copied()
        }
    }

    pub fn map_combo(&self, combo: &str) -> Result<Vec<u32>> {
        let mut vec = Vec::new();
        for key in combo.split(',') {
            let key = key.trim();
            if let Some(code) = self.map(key) {
                vec.push(code);
            } else {
                return Err(anyhow!("Could not find mapping for {} in key combo {}", key, combo));
            }
        }

        Ok(vec)
    }
}

#[derive(Debug)]
struct KeyState {
    min_keycode: Keycode,
    state: Vec<bool>,
    hotkeys: Vec<Vec<u32>>,
    toggle_hotkey: Option<Vec<u32>>,
    hotkeys_enabled: bool,
}

impl KeyState {
    pub fn new(xcb_conn: &xcb::Connection, settings_path: Option<&str>) -> Result<Self> {
        // get keyboard state. documentation: https://xkbcommon.org/doc/current/group__x11.html
        let context = xkbc::Context::new(xkbc::CONTEXT_NO_FLAGS);
        let device_id = xkbc::x11::get_core_keyboard_device_id(xcb_conn);
        let keymap = xkbc::x11::keymap_new_from_device(
            &context,
            xcb_conn,
            device_id,
            xkbc::KEYMAP_COMPILE_NO_FLAGS,
        );

        let min_keycode = keymap.min_keycode();
        let mut hotkeys = Vec::with_capacity(NUM_HOTKEYS - 1);
        let mut toggle_hotkey = None;
        let mapper = Keymapper::new();

        // read LiveSplit settings
        let mut reader = match settings_path {
            Some(s) => XmlReader::from_file(s),
            None => XmlReader::from_file(env::var("HOME")? + "/LiveSplit/settings.cfg"),
        }.context("Failed to open LiveSplit settings")?;
        reader.trim_text(true);
        let mut expect = XmlExpect::None;
        let mut hotkeys_enabled = true;
        let mut buf = Vec::new();
        loop {
            match reader.read_event_into(&mut buf)? {
                XmlEvent::Start(e) => {
                    expect = match e.name().as_ref() {
                        b"SplitKey" | b"ResetKey" | b"SkipKey" | b"UndoKey" | b"PauseKey" | b"SwitchComparisonPrevious" | b"SwitchComparisonNext" => XmlExpect::Hotkey,
                        b"ToggleGlobalHotkeys" => XmlExpect::ToggleHotkey,
                        b"GlobalHotkeysEnabled" => XmlExpect::HotkeysEnabled,
                        _ => XmlExpect::None,
                    };
                }
                XmlEvent::Text(e) => {
                    match expect {
                        XmlExpect::Hotkey => hotkeys.push(mapper.map_combo(e.unescape()?.as_ref())?),
                        XmlExpect::ToggleHotkey => toggle_hotkey = Some(mapper.map_combo(e.unescape()?.as_ref())?),
                        XmlExpect::HotkeysEnabled => hotkeys_enabled = e.unescape()?.trim().eq_ignore_ascii_case("true"),
                        _ => (),
                    }
                }
                XmlEvent::End(_) => expect = XmlExpect::None,
                XmlEvent::Eof => break,
                _ => (),
            }
        }

        let num_keys = KEY.iter().map(|(_, code)| *code).max().unwrap() as usize;

        Ok(Self {
            min_keycode,
            state: vec![false; num_keys],
            hotkeys,
            toggle_hotkey,
            hotkeys_enabled,
        })
    }

    fn check_hotkey(&self, key: u32, hotkey: &[u32]) -> bool {
        hotkey.iter().any(|c| *c == key) && hotkey.iter().all(|c| self.state[*c as usize])
    }

    pub fn handle_key(&mut self, key: u32, is_pressed: bool) -> Vec<Vec<Keycode>> {
        let mut result = Vec::with_capacity(NUM_HOTKEYS);
        self.state[key as usize] = is_pressed;
        if let Some(toggle_hotkey) = self.toggle_hotkey.as_deref() {
            if self.check_hotkey(key, toggle_hotkey) {
                self.hotkeys_enabled = !self.hotkeys_enabled;
                result.push(toggle_hotkey.iter().map(|c| (*c as Keycode) + self.min_keycode).collect());
            }
        }

        if self.hotkeys_enabled {
            for hotkey in &self.hotkeys {
                if self.check_hotkey(key, hotkey) {
                    result.push(hotkey.iter().map(|c| (*c as Keycode) + self.min_keycode).collect());
                }
            }
        }

        result
    }
}

#[derive(Debug, PartialEq)]
enum XmlExpect {
    Hotkey,
    ToggleHotkey,
    HotkeysEnabled,
    None,
}

struct HotkeyListener {
    args: Args,
    xcb_conn: xcb::Connection,
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
            xcb_conn,
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

    pub async fn listen(mut self) -> Result<()> {
        let screen = &self.conn.setup().roots[self.screen_num];
        let window = match self.args.window {
            Some(id) => id,
            None => self.find_window(screen.root)?.ok_or_else(|| anyhow!("LiveSplit window not found"))?,
        };
        println!("Window found: {}", window);

        // find keyboards
        let devices = if self.args.devices.len() > 0 {
            self.args.devices.iter().map(PathBuf::from).collect()
        } else {
            let mut devices = Vec::new();
            let mut entries = read_dir("/dev/input/by-path/").await?;
            while let Some(entry) = entries.next().await {
                let path = entry?.path();
                if path.file_name().map_or(false, |n| n.to_string_lossy().ends_with("-event-kbd")) {
                    devices.push(path);
                }
            }
            devices
        };

        if devices.is_empty() {
            return Err(anyhow!("No keyboard devices found"));
        }

        println!("Keyboards: {:?}", devices);
        let (sender, receiver) = unbounded();
        for device in devices {
            task::spawn(Self::listen_keyboard(sender.clone(), device));
        }

        let key_delay = Duration::from_millis(self.args.key_delay);

        loop {
            let (code, is_pressed) = receiver.recv().await?;
            println!("Key {} = {}", code, is_pressed);
            let active_hotkeys = self.key_state.handle_key(code, is_pressed);
            if !active_hotkeys.is_empty() {
                let focused_window = self.conn.get_input_focus()?.reply()?.focus;
                if focused_window == window {
                    println!("Not sending hotkey because LiveSplit already has focus");
                    continue;
                }
            }

            for keys_to_send in active_hotkeys {
                println!("Sending hotkey {:?}", keys_to_send);
                let mut event_to_send = KeyPressEvent {
                    response_type: KEY_PRESS_EVENT,
                    detail: 0,
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

                for (response_type, mask, do_sleep) in EVENT_SEQUENCE.iter().copied() {
                    for key in &keys_to_send {
                        event_to_send.response_type = response_type;
                        event_to_send.detail = *key as X11rbKeycode;
                        self.conn.send_event(true, window, mask, event_to_send)?.check()?;
                    }
                    if do_sleep {
                        task::sleep(key_delay).await;
                    }
                }
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

#[async_std::main]
async fn main() -> Result<()> {
    let listener = HotkeyListener::new(Args::parse())?;
    listener.listen().await
}
