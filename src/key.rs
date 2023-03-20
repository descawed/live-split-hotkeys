use std::collections::HashMap;
use std::env;

use anyhow::{anyhow, Context, Result};
use input_event_codes_hashmap::KEY;
use quick_xml::events::Event as XmlEvent;
use quick_xml::Reader as XmlReader;
use xkbcommon::xkb as xkbc;
use xkbcommon::xkb::Keycode;

const NUM_HOTKEYS: usize = 8;

#[derive(Debug)]
pub struct Keymapper {
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
                return Err(anyhow!(
                    "Could not find mapping for {} in key combo {}",
                    key,
                    combo
                ));
            }
        }

        Ok(vec)
    }
}

#[derive(Debug, PartialEq)]
enum XmlExpect {
    Hotkey,
    ToggleHotkey,
    HotkeysEnabled,
    None,
}

#[derive(Debug)]
pub struct KeyState {
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
        }
        .context("Failed to open LiveSplit settings")?;
        reader.trim_text(true);
        let mut expect = XmlExpect::None;
        let mut hotkeys_enabled = true;
        let mut buf = Vec::new();
        loop {
            match reader.read_event_into(&mut buf)? {
                XmlEvent::Start(e) => {
                    expect = match e.name().as_ref() {
                        b"SplitKey"
                        | b"ResetKey"
                        | b"SkipKey"
                        | b"UndoKey"
                        | b"PauseKey"
                        | b"SwitchComparisonPrevious"
                        | b"SwitchComparisonNext" => XmlExpect::Hotkey,
                        b"ToggleGlobalHotkeys" => XmlExpect::ToggleHotkey,
                        b"GlobalHotkeysEnabled" => XmlExpect::HotkeysEnabled,
                        _ => XmlExpect::None,
                    };
                }
                XmlEvent::Text(e) => match expect {
                    XmlExpect::Hotkey => hotkeys.push(mapper.map_combo(e.unescape()?.as_ref())?),
                    XmlExpect::ToggleHotkey => {
                        toggle_hotkey = Some(mapper.map_combo(e.unescape()?.as_ref())?)
                    }
                    XmlExpect::HotkeysEnabled => {
                        hotkeys_enabled = e.unescape()?.trim().eq_ignore_ascii_case("true")
                    }
                    _ => (),
                },
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
                result.push(
                    toggle_hotkey
                        .iter()
                        .map(|c| (*c as Keycode) + self.min_keycode)
                        .collect(),
                );
            }
        }

        if self.hotkeys_enabled {
            for hotkey in &self.hotkeys {
                if self.check_hotkey(key, hotkey) {
                    result.push(
                        hotkey
                            .iter()
                            .map(|c| (*c as Keycode) + self.min_keycode)
                            .collect(),
                    );
                }
            }
        }

        result
    }
}

#[cfg(test)]
mod test {
    use crate::key::Keymapper;
    use input_event_codes_hashmap::KEY;

    #[test]
    fn test_key() {
        let mapper = Keymapper::new();
        assert_eq!(mapper.map("G").unwrap(), KEY["G"]);
        assert_eq!(mapper.map("Alt").unwrap(), KEY["LEFTALT"]);
        assert_eq!(mapper.map("NumPad6").unwrap(), KEY["KP6"]);
    }

    #[test]
    fn test_combo() {
        let mapper = Keymapper::new();
        assert_eq!(
            mapper.map_combo("G, Control").unwrap()[..],
            [KEY["G"], KEY["LEFTCTRL"]]
        );
        assert_eq!(
            mapper.map_combo("R, Shift, Alt").unwrap()[..],
            [KEY["R"], KEY["LEFTSHIFT"], KEY["LEFTALT"]]
        );
    }
}
