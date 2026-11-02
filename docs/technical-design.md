# Technical Design: Soundvibes Offline Voice-to-Text CLI

## Overview
SoundVibes is a local Linux dictation CLI with Parakeet Unified English as the
default backend and Phonon-2 as an optional native CPU backend.

## Goals
- Prepare pinned model assets explicitly; inference requires no network.
- Use Vulkan automatically when available, with a CPU path.
- Start/stop capture with final text after release; Parakeet can infer during capture.
- Best-effort latency on CPU.
- Support daemon mode with socket-based control and text injection at the cursor.

## Architecture
- CLI entrypoint loads configuration.
- Evdev hotkey listener controls capture start/stop.
- Audio capture pipeline reads microphone input via `cpal` while the configured key is held.
- A buffer aggregates audio frames for post-recording inference.
- Optional VAD trims trailing silence after release.
- The selected engine runs inference on the captured buffer in the background transcription worker.
- Output stream prints a final transcript.
- Optional daemon service runs continuously and injects text into the focused app.

## Components

### Config
- Load settings from `$XDG_CONFIG_HOME/soundvibes/config.toml`, defaulting to `~/.config/soundvibes/config.toml` when the variable is unset.
- CLI flags complement configuration and override file values when present.
- Defaults are applied if keys are missing.
- Configuration struct shared across pipeline components.
- Use `[output] mode` to select `paste`, `clipboard`, `type`, or `stdout`.
- Use `[hotkey] key` to configure the evdev hold-to-record key.
- Allow overriding the model install path (`model_path`) while keeping a default data directory.

### Audio Capture
- Use `cpal` to select input device and stream 16 kHz mono.
- Convert samples to `f32` normalized range [-1.0, 1.0].
- Capture samples while the configured key is held.

### Buffering
- Store samples for the duration of the recording window.
- Optional chunking to avoid excessive memory for long holds.

### VAD (Voice Activity Detection)
- Optional VAD to trim trailing silence after release.
- Simple energy-based threshold to start; upgradeable later.

### Hotkey Control
- Run `sv daemon start` to start the background service.
- Hold the configured evdev key to start recording; release it to stop and transcribe.
- Monitor udev's `input` subsystem and reconcile `/dev/input/event*` devices when keyboards are added, removed, or replaced.
- Poll evdev and udev file descriptors together, with a 60-second reconciliation fallback for missed notifications.
- Track the hold state per keyboard so disconnecting the last pressed device stops recording without affecting another pressed keyboard.
- Store the socket in `${XDG_RUNTIME_DIR}/soundvibes/sv.sock`.
- Keep socket commands for daemon lifecycle and external start/stop integration.
- Return a versioned JSON response after each command is processed; expose `sv daemon status` for recording state and language.
- Report model reload failures to the requesting client instead of treating a successful socket write as a successful reload.

### Text Output
- Use an output mode to select stdout, clipboard, paste, dotool typing, or low-delay ydotool typing.
- Use `wl-clipboard` to capture and restore the Wayland clipboard.
- Use `dotool` for paste shortcuts and direct typing through `/dev/uinput`.
- Use the existing per-user `ydotoold` service when ydotool output is selected explicitly.
- If configured output is unavailable, fall back to stdout with a warning.

### Daemon Mode
- Long-running process that listens for evdev key press/release events.
- On key press, start capture; on key release, complete transcription.
- On capture completion, either print or inject text based on `mode`.
- Systemd user unit or foreground mode used to manage lifecycle.

### Inference Engine
- Default to Parakeet Unified EN Q8_0 with explicitly prepared assets.
- Run inference on captured audio and return a final transcript.
- Use Q8_0 weights and an explicit CPU path.
- Attempt GPU acceleration automatically; fall back to CPU when no supported GPU backend is detected.

- `engine = "phonon2"` selects the dedicated `crates/phonon2` Rust abstraction.
- Use the original model container and fermion-research 0.2.11 native libraries, pinned by SHA256. The encoder uses ABI 1 and the TDT decoder ABI 3.
- The crate owns model weights and handles, computes the CPU log-mel front end, applies the encoder projection, and renders vocabulary tokens. It requires no Python runtime.
- Phonon-2 is English only and accepts 16 kHz mono PCM. Validate configuration before model loading or capture.
- Native calls are serialized across models. Initialize native thread pools once and retain libraries for process lifetime; model handles and weights are freed on drop.
- Prepare Phonon-2 assets separately with `scripts/prepare-phonon2.py`. `model` identifies the unpacked directory, and native libraries default to its `native` subdirectory, with `phonon2_library_dir` as an override.

### Parakeet Unified English
- `engine = "parakeet"` selects a persistent `sv-parakeet` companion beside `sv`.
- The companion owns the model/session and exchanges local JSON lines over pipes.
  Auto backend permits CPU fallback; explicit CPU and Vulkan are available.
- The worker serializes begin/feed/finalize with transcription jobs. Capture feeds
  immediately and drains release audio before submitting finalization. Final text
  uses the existing output path. Partial hypotheses never inject keystrokes.
- Live mode bypasses offline VAD cuts, retaining full audio for offline retry on
  begin/feed/finalize failure. Cancel discards the session; shutdown finalizes an
  active recording and drains pending results. Pipe IO and replies have timeouts.
- `parakeet_streaming = false` restores offline segmentation. English/16 kHz are
  required. Model preparation and deployment are in [parakeet.md](parakeet.md).

### Live vocabulary
- `vocabulary_dir` / `--vocabulary-dir` defaults to `vocabulary.d` beside `config.toml`. CLI values override TOML.
- The background worker reads visible regular `.txt` files directly in the directory before each decode. It merges in filename/line order, normalizes phrase whitespace, ignores blank/comment lines, and deduplicates without case while retaining the first spelling.
- Additions, edits, atomic replacements and removals affect the next segment or live recording without model reload. Producers should write a temporary file then rename it to `.txt` to publish complete updates. A missing directory means no hints.
- Failed reads, invalid UTF-8 and NUL bytes retain the last complete snapshot with a warning. Directory access runs outside the recording/control loop.
- Phonon-2 compiles the first 25 terms into the upstream sparse word-piece automaton, with a capacity warning for excess terms. The model caches unchanged tables, uses strength 2.0, and disarms native borrowed pointers after each decode. Clearing the list restores unbiased decoding.

- Parakeet receives the combined vocabulary in native run options at stream begin or offline decode. Clearing the directory clears bias on the next recording.

### Model preparation
- On `sv`/`sv daemon start` startup, check for the configured model in the default data directory.
- If `model_path` is provided, download or resolve the model there instead of the default location.

### GPU Backend Selection

### CI and Quality Gates
- GitHub Actions workflow triggers on `pull_request` targeting `main`.
- CI runs a single `mise` task that mirrors local quality gates.
- The task should include Rust best practices: `cargo fmt --check`, `cargo clippy --all-targets --all-features`, `cargo test`, and `cargo build --release`.
- CI fails on any gate failure; local developers can run the same task to reproduce.

### Output Formatting
- `plain`: print final transcript after transcription completes.
- `jsonl`: emit a JSON line with `type`, `text`, `timestamp`.

## Configuration
- Format: TOML.

## Data Flow
1. CLI loads config and model.
2. Configured key press starts audio capture in the daemon.
3. Audio capture stores samples until key release.
4. Optional VAD trims trailing silence.
5. Inference runs on captured audio, returns final text.
6. Output formatter prints or injects final result.

## Error Handling
- Missing model: exit code 2 with message.
- No input device: exit code 3 with message.
- Stream errors: log and exit gracefully.
- Hotkey device access missing: emit actionable guidance for input permissions.

## Validation
- Manual mic test with a configured hold key.
- Validate final transcript after capture stops.
- Confirm offline operation by disconnecting network.
- Validate hold-to-record key press/release behavior.
- Reconnect an external keyboard and confirm its hold key works without restarting the daemon.
- Validate paste output in a focused Wayland editor.

## Open Questions
- VAD threshold calibration on the local microphone.
