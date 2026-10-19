# Technical Design: Soundvibes Offline Voice-to-Text CLI

## Overview
This document describes the technical design for the `sv` CLI that performs offline, start/stop voice-to-text on Linux using whisper.cpp or Phonon-2.

## Goals
- Whisper uses a single binary plus automatic model download to a local path. Optional Phonon-2 uses separately prepared model assets and native CPU libraries.
- Start/stop capture with transcription after capture stops.
- Best-effort latency on CPU.
- Support daemon mode with socket-based control and text injection at the cursor.
- Whisper automatically uses NVIDIA/AMD GPUs when available, falling back to CPU. Phonon-2 uses its native CPU engine.

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
- Config supports `model_language` and `model_size` selection with a default of the small general model.
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
- `engine = "whisper"` is the default, bound to whisper.cpp through Rust FFI.
- Ensure the configured ggml model is downloaded before loading at startup.
- Run inference on captured audio and return a final transcript.
- Use a small quantized model for CPU speed.
- Attempt GPU acceleration automatically; fall back to CPU when no supported GPU backend is detected.

- `engine = "phonon2"` selects the dedicated `crates/phonon2` Rust abstraction.
- Use the original model container and fermion-research 0.2.11 native libraries, pinned by SHA256. The encoder uses ABI 1 and the TDT decoder ABI 3.
- The crate owns model weights and handles, computes the CPU log-mel front end, applies the encoder projection, and renders vocabulary tokens. It requires no Python runtime.
- Phonon-2 is English only and accepts 16 kHz mono PCM. Validate configuration before model loading or capture.
- Native calls are serialized across models. Initialize native thread pools once and retain libraries for process lifetime; model handles and weights are freed on drop.
- Prepare Phonon-2 assets separately with `scripts/prepare-phonon2.py`. `model` identifies the unpacked directory, and native libraries default to its `native` subdirectory, with `phonon2_library_dir` as an override.
- Restart to switch engines. A Phonon-2 daemon rejects the Whisper-size `set-model` command before altering recording or worker state.

### Live vocabulary
- `vocabulary_dir` / `--vocabulary-dir` defaults to `vocabulary.d` beside `config.toml`. CLI values override TOML.
- The background worker reads visible regular `.txt` files directly in the directory before each decode. It merges in filename/line order, normalizes phrase whitespace, ignores blank/comment lines, and deduplicates without case while retaining the first spelling.
- Additions, edits, atomic replacements and removals affect the next segment without model reload. Producers should write a temporary file then rename it to `.txt` to publish complete updates. A missing directory means no hints.
- Failed reads, invalid UTF-8 and NUL bytes retain the last complete snapshot with a warning. Worker model reloads preserve that snapshot. Directory access runs outside the recording/control loop.
- Whisper tokenizes the merged list and keeps the first `n_text_ctx / 2 - 1` tokens, with an explicit capacity warning. The C++ shim sets prompt tokens, carries them across decode windows, and clears prior decode context so removed vocabulary cannot persist.
- Phonon-2 compiles the first 25 terms into the upstream sparse word-piece automaton, with a capacity warning for excess terms. The model caches unchanged tables, uses strength 2.0, and disarms native borrowed pointers after each decode. Clearing the list restores unbiased decoding.

### Model Download
- On `sv`/`sv daemon start` startup, check for the configured model in the default data directory.
- Download the ggml model if missing, based on `model_language` and `model_size` config (defaults to small + general). `large-v3-turbo` is supported as a multilingual-only model size.
- If `model_path` is provided, download or resolve the model there instead of the default location.
- The automatic downloader applies to Whisper. Phonon-2 startup checks prepared assets and reports its setup command when they are missing.

### GPU Backend Selection
- Build whisper.cpp with Vulkan enabled for supported AMD/NVIDIA devices.
- Always enable GPU usage in runtime params; rely on whisper.cpp backend detection to select the first supported device.
- Do not expose GPU selection to the user; if no GPU backend is available, inference continues on CPU.

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
- Example fields: `engine`, `phonon2_library_dir`, `vocabulary_dir`, `model`, `model_path`, `model_size`, `model_language`, `download_model`, `language`, `device`, `sample_rate`, `format`, `vad`, `[output]`, `[hotkey]`.

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
- Validate GPU usage on NVIDIA/AMD systems by checking whisper.cpp startup logs, and verify CPU fallback on systems without a supported GPU backend.

## Open Questions
- VAD threshold calibration on the local microphone.
