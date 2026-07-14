# Acceptance Tests: Soundvibes Offline Voice-to-Text CLI

These tests validate the product behavior for the offline Linux CLI.

## Environment
- Linux x86_64 machine with a working microphone.
- Model file available at `${XDG_DATA_HOME:-~/.local/share}/soundvibes/models/ggml-base.en.bin`.
- Config file at `${XDG_CONFIG_HOME:-~/.config}/soundvibes/config.toml`.
- No network required.
- If available, a machine with a supported NVIDIA/AMD GPU for GPU-acceleration checks.

## Automation notes
- Harness helpers live under `sv::daemon::test_support` and require `cargo test --features test-support`.
- Hardware-dependent tests should be guarded with opt-in env vars:
  - `SV_MODEL_PATH` to point at a local model file for transcription tests.
  - `SV_HARDWARE_TESTS=1` to opt into microphone/GPU checks.
- Automated acceptance tests live in `tests/acceptance.rs` and should map to the AT-xx entries below.
- Run automated acceptance tests with `cargo test --test acceptance` (add `--features test-support` when using mocks).

## Tests

### AT-01: CLI starts with valid model
- Setup: set `model` in config to `${XDG_DATA_HOME:-~/.local/share}/soundvibes/models/ggml-base.en.bin`.
- Command: `sv daemon start`
- Expect: process starts, listens on socket, no error output.
- Pass: exit code is `0` after user stops the process.

### AT-01a: Missing model is auto-downloaded
- Setup: remove `${XDG_DATA_HOME:-~/.local/share}/soundvibes/models/ggml-small.bin`, set `model_size` to `small` and `model_language` to `auto`.
- Command: `sv daemon start`
- Expect: model download occurs before startup completes.
- Pass: model file exists at the default location and daemon starts.

### AT-01b: Language selects model variant
- Setup: set `language = "en"` without `model_language`.
- Command: `sv daemon start`
- Expect: model download uses the `.en` variant when available.
- Pass: model file path resolves to `ggml-<size>.en.bin` when language is `en` and `model_language` is unset; `large-v3-turbo` resolves to `ggml-large-v3-turbo.bin`.

### AT-02: Missing model returns error
- Setup: set `model` in config to `${XDG_DATA_HOME:-~/.local/share}/soundvibes/models/missing.bin` and set `download_model = false`.
- Command: `sv daemon start`
- Expect: error message indicating missing model.
- Pass: exit code is `2`.

### AT-03: Invalid input device
- Setup: set `device` in config to `"nonexistent"`.
- Command: `sv daemon start`
- Expect: error message indicating device not found.
- Pass: exit code is `3`.

### AT-04: Daemon hold-to-record capture
- Setup: set `model` in config to `${XDG_DATA_HOME:-~/.local/share}/soundvibes/models/ggml-base.en.bin`.
- Command: `sv daemon start` with `[hotkey] enabled = true` and a configured key such as `RIGHTCTRL`.
- Action: hold the configured key, speak a short sentence, then release the key.
- Expect: final transcript is printed after key release.
- Pass: final output appears shortly after release.

### AT-05: JSONL output format
- Setup: set `format` in config to `"jsonl"`.
- Command: `sv daemon start` with a configured hold key.
- Action: hold the configured key, speak a short sentence, then release.
- Expect: output lines are valid JSON with `type`, `text`, `timestamp`.
- Pass: JSONL lines parse and include required fields.

### AT-05a: Continuous hold-to-record pause transcription
- Setup: set `vad = "continuous"` and configure `[hotkey] key = "RIGHTCTRL"`.
- Command: `sv daemon start`.
- Action: hold the configured key, speak one sentence, pause without releasing, then speak another sentence.
- Expect: the first sentence is transcribed after the pause while the key is still held.
- Pass: output appears after the pause and recording continues until key release.

### AT-05b: Continuous long-speech timed segmentation
- Setup: set `vad = "continuous"` with default segment timing or explicit `segment_target_ms`, `segment_grace_ms`, `segment_overlap_ms`, and `segment_min_ms`.
- Command: `sv daemon start`.
- Action: hold the configured key and keep speaking past the segment target without a full silence break.
- Expect: SoundVibes starts transcribing a timed segment while audio capture continues.
- Pass: output appears before key release, and the remaining tail is transcribed after release.

### AT-06: Offline operation
- Setup: set `model` in config to `${XDG_DATA_HOME:-~/.local/share}/soundvibes/models/ggml-base.en.bin`.
- Command: disconnect network, run `sv daemon start`, then hold/release the configured key.
- Expect: no network access required.
- Pass: transcription works without network connectivity.

### AT-07: GPU auto-select and CPU fallback
- Setup: run on a machine with a supported NVIDIA/AMD GPU and another machine without GPU support.
- Command: `sv daemon start`.
- Expect: GPU machine logs show a GPU backend selected; CPU-only machine logs show fallback to CPU.
- Pass: transcription succeeds on both, and no manual GPU selection is required.

### AT-09: PR quality gates mirror local checks
- Setup: open a pull request targeting `main`.
- Command: `mise run ci` locally and the CI workflow for the PR.
- Expect: the same set of checks run in both environments.
- Pass: both local and CI runs complete successfully with matching steps.

### AT-11: Paste mode restores clipboard
- Setup: Wayland session with `wl-copy`, `wl-paste`, `dotool`, and `/dev/uinput` access available.
- Command: start daemon with `[output] mode = "paste"` and `restore_clipboard = true`.
- Action: put known content with a known MIME type in the clipboard, dictate text, and let SoundVibes paste it.
- Expect: dictated text is pasted through the configured paste shortcut, and the previous clipboard content is restored with its original MIME type.
- Pass: automated test-support verifies the command sequence; manual KDE verification confirms Klipper does not retain the temporary transcription when the KDE history-suppression hint is honored.
