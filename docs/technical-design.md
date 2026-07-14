# Technical Design: Soundvibes Offline Voice-to-Text CLI

## Overview
This document describes the technical design for the `sv` CLI that performs offline, start/stop voice-to-text on Linux using whisper.cpp with a small quantized model.

## Goals
- Single binary plus automatic model download to a local path.
- Start/stop capture with transcription after capture stops.
- Best-effort latency on CPU.
- Support daemon mode with socket-based control and text injection at the cursor.
- Automatically use NVIDIA/AMD GPUs for inference when available, falling back to CPU.

## Architecture
- CLI entrypoint loads configuration.
- Evdev hotkey listener controls capture start/stop.
- Audio capture pipeline reads microphone input via `cpal` while the configured key is held.
- A buffer aggregates audio frames for post-recording inference.
- Optional VAD trims trailing silence after release.
- whisper.cpp runs inference on the captured buffer.
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
- Store the socket in `${XDG_RUNTIME_DIR}/soundvibes/sv.sock`.
- Keep socket commands for daemon lifecycle and external start/stop integration.

### Text Output
- Use an output mode to select stdout, clipboard, paste, or direct typing.
- Use `wl-clipboard` to capture and restore the Wayland clipboard.
- Use `dotool` for paste shortcuts and direct typing through `/dev/uinput`.
- If configured output is unavailable, fall back to stdout with a warning.

### Daemon Mode
- Long-running process that listens for evdev key press/release events.
- On key press, start capture; on key release, complete transcription.
- On capture completion, either print or inject text based on `mode`.
- Systemd user unit or foreground mode used to manage lifecycle.

### Inference Engine
- whisper.cpp bound via Rust FFI.
- Ensure the configured ggml model is downloaded before loading at startup.
- Run inference on captured audio and return a final transcript.
- Use a small quantized model for CPU speed.
- Attempt GPU acceleration automatically; fall back to CPU when no supported GPU backend is detected.

### Model Download
- On `sv`/`sv daemon start` startup, check for the configured model in the default data directory.
- Download the ggml model if missing, based on `model_language` and `model_size` config (defaults to small + general). `large-v3-turbo` is supported as a multilingual-only model size.
- If `model_path` is provided, download or resolve the model there instead of the default location.

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
- Example fields: `model`, `model_path`, `model_size`, `model_language`, `download_model`, `language`, `device`, `sample_rate`, `format`, `vad`, `[output]`, `[hotkey]`.

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
- Validate paste output in a focused Wayland editor.
- Validate GPU usage on NVIDIA/AMD systems by checking whisper.cpp startup logs, and verify CPU fallback on systems without a supported GPU backend.

## Open Questions
- VAD threshold calibration on the local microphone.
