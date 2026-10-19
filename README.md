# Sinuous

[![crates.io](https://img.shields.io/crates/v/sinuous.svg)](https://crates.io/crates/sinuous)
[![dependency status](https://deps.rs/repo/github/abusch/sinuous/status.svg)](https://deps.rs/repo/github/abusch/sinuous)
[![CI](https://github.com/abusch/sinuous/actions/workflows/rust.yml/badge.svg)](https://github.com/abusch/sinuous/actions/workflows/rust.yml)

Sinuous is a terminal user interface for controlling the Sonos speakers on your
local network.

![screenshot](assets/screenshot.png)

## Features

- See what each group of speakers is playing, and switch between them
- Play, pause and skip tracks, toggle shuffle, repeat and crossfade, and set a
  sleep timer
- Adjust the volume of a group, or of each of its speakers
- Group and ungroup speakers
- Play your Sonos favorites and playlists, skip to any track of the queue, and
  play again what was played recently
- Album art, drawn with the Kitty, iTerm2 or Sixel graphics protocols in
  terminals that support one of them, and with coloured blocks otherwise. It is
  kept in `~/.cache/sinuous/artwork` (or `$XDG_CACHE_HOME/sinuous/artwork`, or
  `%LOCALAPPDATA%\sinuous\artwork` on Windows), up to `album_art_cache_size`
- Themes, and icons for terminals using a [Nerd Font](https://www.nerdfonts.com)

Sinuous talks to the speakers directly, through their local websocket API (using
[sinuous-client](https://github.com/abusch/sinuous-client)), and finds them with
SSDP: no Sonos account is needed, but the speakers need to be on the same
network as your computer.

## Installation

On macOS and Linux, install it with [Homebrew](https://brew.sh):

```sh
brew install abusch/tap/sinuous
```

Or download the binary for your platform from the
[releases page](https://github.com/abusch/sinuous/releases), or install it with
[Cargo](https://rustup.rs):

```sh
cargo install sinuous
```

## Usage

Run `sinuous`. It connects to the first speaker it finds on the network, and
lets you control all the speakers of its household.

To connect to particular speakers, give them with `--device`, separated by
commas. IPv4 addresses work even when the speakers can't be discovered, e.g.
because they are on another network. Names pick the household to control when
several are discovered:

```sh
sinuous --device 192.168.1.10
sinuous --device "Living Room"
```

### Key bindings

| Key | Action |
| --- | --- |
| <kbd>Space</kbd> | Play / pause (or stop) |
| <kbd>p</kbd> / <kbd>n</kbd> | Previous / next track |
| <kbd>s</kbd> / <kbd>r</kbd> | Toggle shuffle / cycle repeat (all, one, off) |
| <kbd>[</kbd> / <kbd>]</kbd> | Volume of the group down / up |
| <kbd>v</kbd> | Volume of the group and of each of its speakers |
| <kbd>.</kbd> | Playback menu: toggle crossfade, and set the sleep timer |
| <kbd>g</kbd> | Switch to another group, from a panel showing what each group is playing |
| <kbd>G</kbd> | Add speakers to or remove speakers from the group |
| <kbd>l</kbd> | The queue of the group, in a panel, to skip to one of its tracks |
| <kbd>h</kbd> | Play again what was played recently, from the same panel |
| <kbd>f</kbd> | Play one of your Sonos favorites, from the same panel |
| <kbd>P</kbd> | Play one of your Sonos playlists, from the same panel |
| <kbd>i</kbd> | System info: the details of each speaker, for troubleshooting |
| <kbd>,</kbd> | Settings |
| <kbd>?</kbd> | Keyboard shortcuts |
| <kbd>q</kbd> | Close what is open, or quit |

Panels and popups show the keys to use in them at their bottom. In the panel
with the queue, the history, the favorites and the playlists, <kbd>Tab</kbd>
switches between them, and <kbd>Enter</kbd> plays what is highlighted. Tracks of
the queue can only be skipped to while the group plays from its queue.

## Configuration

Sinuous reads its settings from `~/.config/sinuous/config.toml` (or
`$XDG_CONFIG_HOME/sinuous/config.toml`, or `%APPDATA%\sinuous\config.toml` on
Windows), if it exists. Use `--config <path>` to read another file instead.
All settings are optional. The `[ui]` settings can also be changed while
running by pressing <kbd>,</kbd>, which saves them to the file:

```toml
[core]
# The speakers to connect to, as IPv4 addresses or names. Speakers are
# discovered via SSDP if not set. `--device` overrides this.
devices = ["192.168.1.10", "Living Room"]
# How much space, in MiB, the album art kept on disk may take, the least
# recently shown being removed first. 0 disables the disk cache.
album_art_cache_size = 64

[ui]
# Whether to show the album art of the current track.
album_art = true
# Whether to show icons instead of labels for actions, play modes and kinds of
# sources, and before the names of music services. Requires a Nerd Font
# (https://www.nerdfonts.com).
nerd_font = false
# The colours to use: "terminal" for the terminal's own, or one of the themes of
# opaline (https://github.com/hyperb1iss/opaline#-builtin-themes): ayu-dark,
# ayu-light, ayu-mirage, catppuccin-frappe, catppuccin-latte,
# catppuccin-macchiato, catppuccin-mocha, dracula, everforest-dark,
# everforest-light, flexoki-dark, flexoki-light, github-dark-dimmed,
# github-light, gruvbox-dark, gruvbox-light, kanagawa-dragon, kanagawa-lotus,
# kanagawa-wave, light-owl, monokai-pro, night-owl, nord, one-dark, one-light,
# palenight, rose-pine, rose-pine-dawn, rose-pine-moon, silkcircuit-dawn,
# silkcircuit-glow, silkcircuit-neon, silkcircuit-soft, silkcircuit-vibrant,
# solarized-dark, solarized-light, tokyo-night, tokyo-night-moon,
# tokyo-night-storm.
theme = "terminal"
```

## Troubleshooting

To get debug logs, run `RUST_LOG=sinuous=debug sinuous`. The logs are written
to `sinuous.log` in the temporary directory: `$TMPDIR` on macOS, `/tmp` on
Linux, and `%TEMP%` on Windows.

## Ideas

- [ ] Seek forward and backward
- [ ] Search for tracks, and manage the queue
- [ ] Show more information about the current track

## License

[MIT](LICENSE)
