# live-split-hotkeys

When running [LiveSplit](https://livesplit.org/) on Linux via Proton, the global hotkeys feature doesn't work.
live-split-hotkeys is a small program that listens for hotkeys and forwards them to LiveSplit. It has a few advantages
over using something like xdotool:

* Not specific to any particular display server (e.g. X or Wayland)
* Reads hotkeys directly from your LiveSplit settings so you don't have to maintain hotkeys in two places
* I can't speak for xbindkeys, but I found Ubuntu custom keyboard shortcuts to be unreliable. For example, the shortcut
  wouldn't trigger if I had other keys pressed at the same time.

## Usage

1. Install and set up [LiveSplit Server](https://github.com/LiveSplit/LiveSplit.Server) if you haven't before. 
2. Make sure you have the "Global hotkeys" option checked in LiveSplit. If you don't, check it and restart LiveSplit.
3. Make sure live-split-hotkeys has access to your keyboard devices in `/dev/input`. The easiest way would be to run it
   with `sudo`.
4. Start live-split-hotkeys after starting LiveSplit and let it run. You'll probably need to tell it where to find
   LiveSplit's settings.cfg with the `--settings` option. You can kill it with ctrl+c when you're done. Use
   `live-split-hotkeys -h` for help.

## Limitations

* Only tested on Ubuntu 22.10 with a US English keyboard. I can't currently confirm whether it will work on other
  distros or with other keyboard layouts.
* No controller support.
* Doesn't support the "switch comparison" hotkeys.
* If live-split-hotkeys is running while the LiveSplit window has focus, hotkeys will be double-triggered.
* If you change your hotkeys, you'll need to restart LiveSplit and then live-split-hotkeys. This is because
  live-split-hotkeys reads your hotkeys from LiveSplit's settings.cfg on startup, and LiveSplit only updates that file
  when it exits.
* If you have multiple keyboard devices (which can include things like gaming mice with programmable buttons), weird
  things might happen if you press the same key(s) on more than one at the same time.

## Troubleshooting

* **Failed to open LiveSplit settings**: LiveSplit settings are assumed to be in `$HOME/LiveSplit/settings.cfg`. If
  that's not where you have LiveSplit (and keep in mind `$HOME` will be `/root` if running the command with `sudo`),
  pass a different path with the `--settings` option.
* **No keyboard devices found**: Any device in `/dev/input/by-path/` that ends with `-event-kbd` is considered to be a
  keyboard. If your keyboard devices don't match that pattern, use the `--devices` option to tell it which device(s) to
  watch.
* **Could not connect to LiveSplit server**: Make sure LiveSplit is running and you've started the server (see the
  [LiveSplit Server setup instructions](https://github.com/LiveSplit/LiveSplit.Server#setup)).
* **could not open /dev/input/...**: You don't have permission to the keyboard device file(s). The easiest way
  to fix this is to run it with `sudo`. I wouldn't recommend giving your user account permission to those files as that
  would allow any program you run to keylog you. You're of course free to find any other solution that fits your
  security preferences. Personally, I use `setcap` to give the executable the `CAP_DAC_READ_SEARCH` capability so I
  don't have to run it as root.