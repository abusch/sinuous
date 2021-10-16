# Sinuous

Sinuous is a simple TUI for controlling local Sonos speakers.

It currently connects to the first speaker that is currently playing (or the first speaker found if no speaker is currently playing), displays the current track, and the current queue.

Note: `sinuous` directly talks to the Sonos speakers via their local upnp interface, and the speakers are discovered via the SSDP protocol. This means your Sonos speakers need to be on the same network (or visible from your current network).

## Key bindings
- <kbd>Space</kbd>: Play / Pause
- <kbd>n</kbd>: Next track
- <kbd>p</kbd>: Previous track
- <kbd>q</kbd>: Quit

## To run
Install a recent Rust toolchain via [rustup](https://rustup.rs), if you don't already have one, then simply run `cargo run`.

To get debug logs, run `RUST_LOG="sinuous=debug" cargo run`. The logs can be found in `/tmp/sinuous.log`.

## Todo
- [ ] Allow switching between speakers
- [ ] Support more actions (seek forward, backward, change playing mode, volume...)
- [ ] Display play/pause indicator as well as current play mode (shuffle+repeat)
- [ ] Allow searching for tracks and modify the queue
- [ ] Allow customizing colours
- [ ] Allow specifying speaker to connect to as a command line argument
