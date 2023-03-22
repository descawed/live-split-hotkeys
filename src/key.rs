use std::collections::HashMap;
use std::env;

use anyhow::{anyhow, Context, Result};
use enum_map::{Enum, EnumMap};
use input_event_codes_hashmap::KEY;
use quick_xml::events::Event as XmlEvent;
use quick_xml::Reader as XmlReader;

#[derive(Debug, Enum, PartialEq, Clone, Copy)]
pub enum Hotkey {
    SplitKey,
    ResetKey,
    SkipKey,
    UndoKey,
    PauseKey,
    ToggleGlobalHotkeys,
}

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
    Hotkey(Hotkey),
    HotkeysEnabled,
    None,
}

#[derive(Debug)]
pub struct KeyState {
    state: Vec<bool>,
    hotkeys: EnumMap<Hotkey, Vec<u32>>,
    hotkeys_enabled: bool,
}

impl KeyState {
    pub fn new(settings_path: Option<&str>, profile: &str) -> Result<Self> {
        let mut hotkeys = EnumMap::default();
        let mapper = Keymapper::new();
        let profile_bytes = profile.as_bytes();

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
        let mut in_profile = false;
        loop {
            match reader.read_event_into(&mut buf)? {
                XmlEvent::Start(e) => {
                    expect = match e.name().as_ref() {
                        b"HotkeyProfile" => {
                            if let Some(name_attr) = e
                                .attributes()
                                .find(|a| a.as_ref().map_or(false, |a| a.key.as_ref() == b"name"))
                                .map(|r| r.unwrap())
                            {
                                in_profile = name_attr.value.as_ref() == profile_bytes;
                            } else {
                                in_profile = false;
                            }
                            XmlExpect::None
                        }
                        b"SplitKey" if in_profile => XmlExpect::Hotkey(Hotkey::SplitKey),
                        b"ResetKey" if in_profile => XmlExpect::Hotkey(Hotkey::ResetKey),
                        b"SkipKey" if in_profile => XmlExpect::Hotkey(Hotkey::SkipKey),
                        b"UndoKey" if in_profile => XmlExpect::Hotkey(Hotkey::UndoKey),
                        b"PauseKey" if in_profile => XmlExpect::Hotkey(Hotkey::PauseKey),
                        b"ToggleGlobalHotkeys" if in_profile => {
                            XmlExpect::Hotkey(Hotkey::ToggleGlobalHotkeys)
                        }
                        b"GlobalHotkeysEnabled" if in_profile => XmlExpect::HotkeysEnabled,
                        _ => XmlExpect::None,
                    };
                }
                XmlEvent::Text(e) => match &expect {
                    XmlExpect::Hotkey(hotkey) => {
                        hotkeys[*hotkey] = mapper.map_combo(e.unescape()?.as_ref())?
                    }
                    XmlExpect::HotkeysEnabled => {
                        hotkeys_enabled = e.unescape()?.trim().eq_ignore_ascii_case("true")
                    }
                    _ => (),
                },
                XmlEvent::End(e) => {
                    if e.name().as_ref() == b"HotkeyProfile" {
                        in_profile = false;
                    }
                    expect = XmlExpect::None;
                }
                XmlEvent::Eof => break,
                _ => (),
            }
        }

        let num_keys = KEY.iter().map(|(_, code)| *code).max().unwrap() as usize;

        Ok(Self {
            state: vec![false; num_keys],
            hotkeys,
            hotkeys_enabled,
        })
    }

    fn check_hotkey(&self, key: u32, hotkey: &[u32]) -> bool {
        hotkey.iter().any(|c| *c == key) && hotkey.iter().all(|c| self.state[*c as usize])
    }

    pub fn handle_key(&mut self, key: u32, is_pressed: bool) -> EnumMap<Hotkey, bool> {
        let mut result = EnumMap::default();
        self.state[key as usize] = is_pressed;

        for (hotkey, combo) in &self.hotkeys {
            let is_active = self.check_hotkey(key, combo);
            if is_active && hotkey == Hotkey::ToggleGlobalHotkeys {
                self.hotkeys_enabled = !self.hotkeys_enabled;
                if !self.hotkeys_enabled {
                    return EnumMap::default();
                }
            }

            if !self.hotkeys_enabled {
                continue;
            }

            result[hotkey] = is_active;
        }

        result
    }
}

#[cfg(test)]
mod tests {
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
