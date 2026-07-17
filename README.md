# SoundVibes

SoundVibes (`sv`) is a personal, offline speech-to-text CLI for Linux. It records while a configured evdev key is held, transcribes locally with whisper.cpp, and pastes or prints the result when the key is released.

## Build and install

Initialize the whisper.cpp submodule and install the local checkout with Cargo:

```bash
git submodule update --init --recursive
mise run prepare-dev
cargo install --path .
```

`mise run prepare-dev` currently supports Debian/Ubuntu and Arch-family systems. The binary expects Linux x86_64, ALSA, libudev, and optionally Vulkan for GPU acceleration.

## Configure

Create `$XDG_CONFIG_HOME/soundvibes/config.toml`, or `~/.config/soundvibes/config.toml` when `XDG_CONFIG_HOME` is unset. Application defaults live in the CLI; a minimal personal configuration only needs a hotkey:

```toml
[hotkey]
enabled = true
key = "RIGHTCTRL"
```

SoundVibes reads `/dev/input/event*`, so the user running it must have permission to read keyboard event devices. Use `evtest` to find a key name and configure the part after `KEY_`.

Useful optional settings:

```toml
language = "en"
model_size = "small"
model_language = "auto"
vad = "continuous"

[output]
mode = "paste"
paste_keys = "ctrl+v"
```

Keep root settings before `[output]` and `[hotkey]`; TOML keys following a table header belong to that table.

### Universal terminal paste and safe zero-delay typing

Most graphical applications accept `Shift+Insert` as a clipboard paste shortcut. Konsole supports it by default, while Ghostty needs an explicit binding so it uses the regular clipboard instead of the selection clipboard. Configure SoundVibes and Ghostty together with:

```bash
contrib/setup-universal-paste
```

The script requires `keyd` and `ydotool`. It configures `AltGr+Right Ctrl` as a keyd chord that emits only `F24`, selects `F24` as the Soundvibes hold key, enables the zero-delay `ydotool` backend, starts both input daemons, and restarts Soundvibes when its user service is active. Because applications receive `F24` instead of either source modifier, continuous transcription cannot accidentally trigger shortcuts such as `Ctrl+Q` while the chord is held.

Existing Soundvibes and Ghostty settings are preserved, and the script is safe to run repeatedly. It also retains `Shift+Insert` as a clipboard fallback. Reload Ghostty with `Ctrl+Shift+,` or restart it after running the script.

Switch `[output] mode` back to `"paste"` to restore clipboard paste. The ydotool backend uses zero key delay and zero key hold. It is very fast, but unlike clipboard paste it follows the active keyboard layout and has limited Unicode support.

## Run

```bash
sv daemon start
```

Hold the configured key while speaking, then release it to finish the recording. Continuous VAD can emit segments during longer holds when it detects pauses.

Inspect or control the running daemon with acknowledged commands:

```bash
sv daemon status
sv daemon set-model --size small --model-language en
sv daemon stop
```

`status` reports the current recording state and transcription language. Model changes return only after loading succeeds or fails.

Output modes:

- `paste` (default): temporarily copies text, pastes with `dotool`, then restores the clipboard.
- `clipboard`: leaves the transcript on the clipboard.
- `type`: types text directly with `dotool`.
- `ydotool`: types with zero delay through the existing `ydotoold` user service.
- `stdout`: prints transcripts in the daemon terminal.

Paste and clipboard modes require `wl-clipboard`; automatic paste and type modes require `dotool` plus `/dev/uinput` access. Ydotool mode requires the `ydotool` client and a running `ydotoold` user service.

To run as a user service after `cargo install`, copy the supplied unit:

```bash
mkdir -p ~/.config/systemd/user
cp contrib/sv.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now sv.service
```

## Development

Run the complete local quality gate with:

```bash
mise run ci
```

Focused commands and hardware-test flags are documented in `AGENTS.md` and `docs/acceptance-tests.md`.

## License

GNU General Public License v3.0; see `LICENSE`.
