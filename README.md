# live-split-hotkeys

When running [LiveSplit](https://livesplit.org/) on Linux via Proton, the global hotkeys feature doesn't work.
live-split-hotkeys is a small program that listens for hotkeys and forwards them to the LiveSplit window.

## Usage

Make sure you have the "Global hotkeys" option checked in LiveSplit. If you don't, check it and restart LiveSplit.
You'll also need to make sure the program has access to your keyboard devices in `/dev/input`. The easiest way would be
to run it with `sudo`. Then start the program after starting LiveSplit and let it run. You can kill it with ctrl+c when
you're done. Use `live-split-hotkeys -h` for help.

## Limitations

* Only tested on Ubuntu 22.10 with a US English keyboard. I can't currently confirm whether it will work on other
  distros or with other keyboard layouts.
* No controller support.
* Uses X11 and has no awareness of Wayland. Maybe it works with XWayland? To be honest, I don't know that much about how
  Wayland works. I'd be interested to hear feedback from Wayland users.
* If you change your hotkeys, you'll need to restart LiveSplit and then live-split-hotkeys. This is because
  live-split-hotkeys reads your hotkeys from LiveSplit's settings.cfg on startup, and LiveSplit only updates that file
  when it exits. This issue could be avoided if I were to forward all key events to LiveSplit instead of only hotkey
  events, but I wasn't sure if that would have unintended consequences (i.e. performing non-hotkey actions in
  LiveSplit). I'm open to suggestions on better ways to handle this.
* If you have multiple keyboard devices (which can include things like gaming mice with programmable buttons), weird
  things might happen if you press the same key(s) on more than one at the same time.
* If you start the program when LiveSplit isn't running, it can occasionally find the wrong window, for instance a
  Nautilus window opened to a folder called LiveSplit. Just make sure LiveSplit is running first.

## Troubleshooting

* **Failed to open LiveSplit settings**: LiveSplit settings are assumed to be in `$HOME/LiveSplit/settings.cfg`. If
  that's not where you have LiveSplit, pass a different path with the `--settings` option.
* **No keyboard devices found**: Any device in `/dev/input/by-path/` that ends with `-event-kbd` is considered to be a
  keyboard. If your keyboard devices don't match that pattern, use the `--devices` option to tell it which device(s) to
  watch.
* **LiveSplit window not found**: Make sure LiveSplit is running. If it still doesn't find it for some reason, you can
  explicitly tell it which window is the LiveSplit window with the `--window` option using e.g. xdotool.
* **could not open /dev/input/...**: You don't have permission to the keyboard device file(s). The easiest way
  to fix this is to run it with `sudo`. I wouldn't recommend giving your user account permission to those files as that
  would allow any program you run to keylog you. You're of course free to find any other solution that fits your
  security preferences.

## Build

You'll need to install the following packages:

* libxkbcommon-dev
* libxcb-xkb-dev
* libxkbcommon-x11-dev

Then just run `cargo build`.