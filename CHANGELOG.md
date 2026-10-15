# Changelog

All notable changes to this project will be documented in this file.

## [0.2.0](https://github.com/abusch/sinuous/compare/v0.1.6...v0.2.0) - 2026-10-15

### Added

- Show the time remaining in the current track rather than its duration, like the Sonos apps
- Show the icon of the music service before its name when icons are enabled
- Show the kind of source as an icon when icons are enabled
- Don't show the album of the current track: its source is usually more relevant
- Show a placeholder where the album art goes when there is none to show
- Close what is open with q, rather than quitting
- Adjust the volume with h and l in the volume mixer again, and show the help with ? only
- Merge the volume changes waiting to run, so that holding a key doesn't fill the command queue
- Adjust the volume in steps of 1
- Connect to the speakers before starting the UI, with a spinner in the terminal
- Show the model of each speaker in the group editor and the volume mixer
- Edit the speakers of a group in the panel on the left, like the groups
- Show the groups in a panel on the left, with what they are playing
- Support themes
- Move the shortcuts to the header, with help and configuration popups
- Show volumes as sliders, and line the progress bar up with them
- Optionally show Nerd Font icons instead of labels
- Add a configuration file
- Show album art
- Grey out the actions the current source doesn't support
- Show the source, play modes and explicit tag of the current track
- Replace the queue and favorites with a bigger now playing view
- Adjust the volume of individual speakers
- Support grouping and ungrouping speakers
- Switch groups with a picker instead of tabs
- Use events instead of polling for playback updates
- Connect to the first speaker that answers discovery
- Replace sonor with sinuous-client

### Fixed

- Scroll the panel on the left as little as possible, rather than keeping the highlighted item at the bottom
- Save the configuration file atomically
- Don't turn the volume down with h in the volume mixer, which is the key for help
- Don't panic lining up the theme with the settings if its icon is wider
- Don't keep room for album art that failed to load
- Report failed updates from a speaker once, naming it, rather than at every retry
- Only act on key presses, not on releases
- Show why the app stopped after restoring the terminal
- Follow the selected group's coordinator into the group it joins
- Keep the volume slider's handle at the end of its bar

### Other

- Remove the old release hook: release-plz writes the changelog
- Refresh the README and its screenshot for 0.2.0
- Describe the crate for crates.io, and only package what is needed to build it
- Build the release binaries once release-plz has created the release
- Bump the minor version for new features while sinuous is below 1.0
- Describe how the selected group is followed as real speakers behave
- Tick off customizing colours, now that themes are supported
- Split the view into modules for input, what is playing, the panel and the popups
- Keep what is open in one enum, rather than in fields that could all be set
- Work out the command for a key and whether it is supported in one match
- Only look up the coordinator of a group for the events that need it
- Build track information in one place, and only find the image of the current track
- Simplify how the previewed theme is kept
- Update dependencies
- Rely on sinuous-client to restore subscriptions
- Upgrade sinuous-client to 0.2.0
- Move UI state out of the Sonos service
- Use sinuous-client from crates.io
- Remove unused dependencies
- *(deps)* bump clap from 4.5.54 to 4.6.0

## [0.1.6](https://github.com/abusch/sinuous/compare/v0.1.5...v0.1.6) - 2026-01-20

### Added

- add Sonos favorites playlists support
- *(ci)* Add workflow to build binaries

### Other

- simplify display of help text and upgrade to ratatui 0.30
- Fix some clippy warnings
- Update dependencies
- *(deps)* bump actions/checkout from 5 to 6
- *(deps)* bump actions/checkout from 4 to 5
- split out app code into App struct
- Use ratatui init() and restore()
- *(ci)* Make dependabot track github actions

## [0.1.5](https://github.com/abusch/sinuous/compare/v0.1.4...v0.1.5) - 2025-06-13

### Other

- fix CI link in README.md
- Update more dependencies...
- Switch CI to release-plz
- Upgrade dependencies
- Update dependabot.yml
- Upgrade ratatui/crossterm
- Don't rename ratatui package
- run cargo update
- *(deps)* bump human-panic from 1.2.3 to 2.0.0
- fix some clippy warnings
- Upgrade ratatui
- Update ci.yml/cd.yml

## [0.1.4] - 2023-09-17

### Bug Fixes

- Update sonor and other dependencies

### Miscellaneous Tasks

- Upgrade some dependencies
- Upgrade dependencies

## [0.1.3] - 2022-05-18

### Bug Fixes

- Make sure the ratio of track played stays between 0 and 1

### Documentation

- Add screenshot to README file
- Update README file
- Update screenshot

### Features

- Manage groups instead of speakers
- Remove dead code

### Miscellaneous Tasks

- Don't swallow error if main loop exits abnormally
- Formatting
- Improve logging and display of panic info

### Build

- Strip symbols from binary
- Bump crossterm from 0.23.1 to 0.23.2
- Bump tracing-subscriber from 0.3.9 to 0.3.10
- Bump clap from 3.1.6 to 3.1.8
- Bump tracing-subscriber from 0.3.10 to 0.3.11
- Bump tracing from 0.1.32 to 0.1.33
- Bump tracing from 0.1.33 to 0.1.34
- Bump clap from 3.1.8 to 3.1.9
- Bump anyhow from 1.0.56 to 1.0.57
- Bump clap from 3.1.9 to 3.1.12
- Bump tui from 0.17.0 to 0.18.0
- Bump tokio from 1.17.0 to 1.18.0
- Bump tokio from 1.18.0 to 1.18.2
- Bump clap from 3.1.12 to 3.1.18

## [0.1.2] - 2022-03-25

### Ci

- Move pre-release hook to script file

## [0.1.1] - 2022-03-25

### Bug Fixes

- Don't harcode version number in UI

## [0.1.0] - 2022-03-24

### Features

- Add play/pause symbols to the play bar
- Display current volume in title bar
- Allow controlling the volume of the current speaker
- Highlight the currently playing track

### Miscellaneous Tasks

- Remove unnecessary braces
- Add more metadata to Cargo.toml
- Remove redundant license-file key

### Refactor

- Split render_ui into smaller functions

### Styling

- Apply cargo fmt globally

### Ci

- Steal workflow files from ncspot

<!-- generated by git-cliff -->
