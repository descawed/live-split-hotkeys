use std::mem::size_of;
use std::ptr::addr_of;

use anyhow::{anyhow, Context, Result};
use async_std::channel::{unbounded, Receiver, Sender};
use async_std::fs::{read_dir, File};
use async_std::io::{ReadExt, WriteExt};
use async_std::net::TcpStream;
use async_std::path::PathBuf;
use async_std::prelude::StreamExt;
use async_std::task;
use clap::Parser;
use futures::future;
use input_event_codes_hashmap::EV;
use libc::input_event;

mod key;
use key::*;

/// Listen for LiveSplit hotkeys
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Path to LiveSplit's settings.cfg where the hotkeys will be read from
    #[arg(short, long)]
    settings: Option<String>,
    /// Name of the hotkey profile to use
    #[arg(short = 'f', long, default_value_t = String::from("Default"))]
    profile: String,
    /// Hostname or IP address where the LiveSplit server is running
    #[arg(short = 'o', long, default_value_t = String::from("localhost"))]
    host: String,
    /// Port that the LiveSplit server is listening on
    #[arg(short, long, default_value_t = 16834)]
    port: u16,
    /// Path to the keyboard device file(s) to read from
    #[arg(short, long)]
    devices: Vec<String>,
    /// Display debug information. Specify twice to show every key event.
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,
}

struct HotkeyListener {
    args: Args,
    key_state: KeyState,
}

impl HotkeyListener {
    pub fn new(args: Args) -> Result<Self> {
        let key_state = KeyState::new(args.settings.as_deref(), args.profile.as_str())?;
        Ok(Self { args, key_state })
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

    async fn listen_keys(mut self, receiver: Receiver<(u32, bool)>) -> Result<()> {
        let mut conn = TcpStream::connect(format!("{}:{}", self.args.host, self.args.port))
            .await
            .context("Could not connect to LiveSplit server")?;
        let mut paused = false;

        loop {
            let (code, is_pressed) = receiver.recv().await?;
            if self.args.verbose > 1 {
                println!("Key {} = {}", code, is_pressed);
            }
            let active_hotkeys = self.key_state.handle_key(code, is_pressed);

            for hotkey in active_hotkeys
                .into_iter()
                .filter_map(|(hotkey, is_active)| is_active.then_some(hotkey))
            {
                if self.args.verbose > 0 {
                    println!("Sending hotkey {:?}", hotkey);
                }
                let command: &'static [u8] = match hotkey {
                    Hotkey::SplitKey => b"startorsplit\r\n",
                    Hotkey::ResetKey => b"reset\r\n",
                    Hotkey::SkipKey => b"skipsplit\r\n",
                    Hotkey::UndoKey => b"unsplit\r\n",
                    Hotkey::PauseKey => {
                        let command: &'static [u8] =
                            if paused { b"resume\r\n" } else { b"pause\r\n" };
                        paused = !paused;
                        command
                    }
                    _ => continue,
                };

                conn.write_all(command).await?;
            }
        }
    }

    pub async fn listen(self) -> Result<()> {
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
        tasks.push(task::spawn(self.listen_keys(receiver)));
        future::try_join_all(tasks).await.map(|_| ())
    }
}

#[async_std::main]
async fn main() -> Result<()> {
    let listener = HotkeyListener::new(Args::parse())?;
    listener.listen().await
}
