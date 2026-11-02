# SoundVibes

SoundVibes (`sv`) is a personal, offline speech-to-text CLI for Linux. It records while a configured evdev key is held, transcribes locally with whisper.cpp or Phonon-2, and pastes or prints the result when the key is released.

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

To compare transcription with text inserted by an output backend, enable transcript debug logging:

```toml
debug_transcripts = true
```

The equivalent CLI flag is `--debug-transcripts`. Each completed transcript is written to stderr as one escaped line before output processing, so a systemd service records it in the journal. Inspect recent entries with `journalctl --user -u sv.service`. This mode logs dictated text in plain text and is disabled by default.

### Custom vocabulary

Create `$XDG_CONFIG_HOME/soundvibes/vocabulary.d`, or
`~/.config/soundvibes/vocabulary.d` when `XDG_CONFIG_HOME` is unset. Each application
can manage its own UTF-8 `.txt` file in that directory, with one word or phrase per
line. For example, `projects.txt` could contain:

```text
# Project and product names
SoundVibes
PostgreSQL
Ada Lovelace
```

The daemon combines the files before every transcription, including segments
produced while the recording key stays held. Add, edit, replace, or delete files
while it runs; the next segment uses the updated list without restarting or
reloading the model. A missing directory is empty and can be created later.

Files are read in filename order and lines in file order. Duplicate terms are
matched without case, with the first spelling retained. Blank lines and lines
starting with `#` are ignored; spaces inside phrases are normalized. `C#` is a
valid term. Only regular, visible `.txt` files directly in the directory are read.
Subdirectories, symlinks and temporary files such as `projects.txt.tmp` are ignored.

Write to a temporary file and rename it over your application's `.txt` file to
publish a complete update. An unreadable file, invalid UTF-8 or a NUL byte causes
a warning; dictation continues with the previous complete list until reading
succeeds again. Deleting all files or the directory clears the vocabulary.

Override the directory with `--vocabulary-dir /absolute/path` or a root TOML key:

```toml
vocabulary_dir = "/home/you/vocabulary.d"
```

The CLI overrides TOML. Changing the directory setting requires a daemon restart;
changing files inside it does not.

Whisper uses vocabulary as a text prompt. It keeps the first tokens that fit its
prompt budget, typically 223 tokens. Phonon-2 uses native hotword bias at strength
2.0 and accepts the first 25 distinct terms. Both engines warn when the combined
list exceeds their capacity. Use filename and line order to put priority terms
first. Vocabulary guides recognition; it does not guarantee a particular spelling.

For Phonon-2, rerun `scripts/prepare-phonon2.py` with the documented dependencies
to install the pinned 0.2.11 native libraries. Existing 0.2.6 libraries do not have
the required hotword API. The model weights are unchanged.

### Universal terminal paste and safe low-delay typing

Most graphical applications accept `Shift+Insert` as a clipboard paste shortcut. Konsole supports it by default, while Ghostty needs an explicit binding so it uses the regular clipboard instead of the selection clipboard. Configure SoundVibes and Ghostty together with:

```bash
contrib/setup-universal-paste
```

The script requires `keyd` and `ydotool`. It configures `AltGr+Right Ctrl` as a keyd chord that emits only `F24`, selects `F24` as the Soundvibes hold key, enables the low-delay `ydotool` backend, starts both input daemons, and restarts Soundvibes when its user service is active. Because applications receive `F24` instead of either source modifier, continuous transcription cannot accidentally trigger shortcuts such as `Ctrl+Q` while the chord is held.

Existing Soundvibes and Ghostty settings are preserved, and the script is safe to run repeatedly. It also retains `Shift+Insert` as a clipboard fallback. Reload Ghostty with `Ctrl+Shift+,` or restart it after running the script.

Switch `[output] mode` back to `"paste"` to restore clipboard paste. The ydotool backend uses a 3 ms key delay and 3 ms key hold. This prevents applications from dropping or reordering synthetic key events while keeping insertion fast. Unlike clipboard paste, ydotool follows the active keyboard layout and has limited Unicode support.

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

### Phonon-2

Whisper is the default. Phonon-2 is a second option for English transcription on
Linux CPUs. Prepare the original model and pinned native libraries once:

```bash
uv run --with zstandard python scripts/prepare-phonon2.py
sv daemon start --engine phonon2
```

Setup requires Python 3.11+; inference runs entirely in Rust and the native CPU
libraries. Assets go to `$XDG_DATA_HOME/soundvibes/models/phonon2`, defaulting to
`~/.local/share/soundvibes/models/phonon2`. After setup, transcription works offline.

To select it in your configuration, put these settings before any table headers:

```toml
engine = "phonon2"
language = "en"
sample_rate = 16000
```

Use `--model DIRECTORY` or `model = "/absolute/model/directory"` for a custom
unpacked model location. Native libraries default to `DIRECTORY/native`; override
with `--phonon2-library-dir DIRECTORY` or `phonon2_library_dir` in TOML.

Phonon-2 supports `language = "en"` and `"auto"`, both using its English decoder.
It requires 16 kHz mono audio and runs on CPU. `model_size`, `model_language` and
`download_model` apply to Whisper. Phonon-2 uses the separate setup command;
startup reports missing assets without downloading a Whisper model. Restart the
daemon to change engines. `daemon set-model` changes Whisper sizes and is rejected
by a Phonon-2 daemon without interrupting a recording.

The dedicated [phonon2 crate](crates/phonon2/README.md) documents its API, native
library provenance, and reproducible real-audio/Python comparison tests.

Output modes:

- `paste` (default): temporarily copies text, pastes with `dotool`, then restores the clipboard.
- `clipboard`: leaves the transcript on the clipboard.
- `type`: types text directly with `dotool`.
- `ydotool`: types with a 3 ms key delay and hold through the existing `ydotoold` user service.
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

Parakeet Unified English 0.6B setup and local streaming: [docs/parakeet.md](docs/parakeet.md).
