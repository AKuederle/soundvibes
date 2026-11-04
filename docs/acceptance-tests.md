# Acceptance Tests: Soundvibes Offline Voice-to-Text CLI

These tests validate the product behavior for the offline Linux CLI.

## Environment
- Linux x86_64 machine with a working microphone.
- `<data-home>` means `$XDG_DATA_HOME` when set, or `~/.local/share` when it is unset.
- `<config-home>` means `$XDG_CONFIG_HOME` when set, or `~/.config` when it is unset.
- Model file available at `<data-home>/soundvibes/models/parakeet-unified-en-0.6b-Q8_0.gguf`.
- Config file at `<config-home>/soundvibes/config.toml`.
- No network required.
- If available, a machine with a supported NVIDIA/AMD GPU for GPU-acceleration checks.

## Automation notes
- Harness helpers live under `sv::daemon::test_support` and require `cargo test --features test-support`.
- Hardware-dependent tests should be guarded with opt-in env vars:
  - `SV_PARAKEET_MODEL` to point at a local model file for transcription tests.
  - `SV_HARDWARE_TESTS=1` to opt into microphone/GPU checks.
- Automated acceptance tests live in `tests/acceptance.rs` and should map to the AT-xx entries below.
- Run automated acceptance tests with `cargo test --test acceptance` (add `--features test-support` when using mocks).
- Phonon-2 real-model tests require `PHONON2_MODEL_DIR` and `PHONON2_LIBRARY_DIR`; ordinary runs skip when the model variable is unset. Explicitly configured missing assets fail. Use release mode for large-model tests.

## Tests

### AT-01: CLI starts with valid model
- Setup: set `model` in config to `<data-home>/soundvibes/models/parakeet-unified-en-0.6b-Q8_0.gguf`.
- Command: `sv daemon start`
- Expect: process starts, listens on socket, no error output.
- Pass: exit code is `0` after user stops the process.

### AT-02: Missing model returns error
- Setup: set `model` in config to `<data-home>/soundvibes/models/missing.bin`.
- Command: `sv daemon start`
- Expect: error message indicating missing model.
- Pass: exit code is `2`.

### AT-03: Invalid input device
- Setup: set `device` in config to `"nonexistent"`.
- Command: `sv daemon start`
- Expect: error message indicating device not found.
- Pass: exit code is `3`.

### AT-04: Daemon hold-to-record capture
- Setup: set `model` in config to `<data-home>/soundvibes/models/parakeet-unified-en-0.6b-Q8_0.gguf`.
- Command: `sv daemon start` with `[hotkey] enabled = true` and a configured key such as `RIGHTCTRL`.
- Action: hold the configured key, speak a short sentence, then release the key.
- Expect: final transcript is printed after key release.
- Pass: final output appears shortly after release.

### AT-04a: Hotkey keyboard reconnect
- Setup: set `SV_HARDWARE_TESTS=1` on Linux with writable `/dev/uinput` and readable `/dev/input/event*` devices.
- Command: `cargo test --test acceptance at04a_hotkey_keyboard_reconnects_without_listener_restart`.
- Action: the test creates a virtual keyboard, verifies its hold key, removes it, creates a replacement, and verifies the replacement without restarting the listener.
- Expect: both virtual keyboards produce start and stop control events.
- Pass: the replacement keyboard is active within the test timeout; the test skips gracefully when `/dev/uinput` is unavailable.

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
- Setup: set `model` in config to `<data-home>/soundvibes/models/parakeet-unified-en-0.6b-Q8_0.gguf`.
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

### AT-12: Daemon commands are acknowledged
- Setup: start the control socket and daemon loop with test-support audio and transcription adapters.
- Command: request `status`, then request `stop`.
- Expect: status returns versioned JSON with `state = "idle"` and `language = "en"`; stop returns success.
- Pass: each client receives the daemon loop's response rather than inferring success from the socket write.

### AT-13: Universal paste and safe typing setup
- Setup: install `keyd` and `ydotool`, then use empty temporary `<config-home>` and keyd configuration directories.
- Command: run `contrib/setup-universal-paste`.
- Expect: keyd maps the `rightalt+rightcontrol` chord to `F24`; Soundvibes uses `F24` with the low-delay `ydotool` backend; Ghostty maps `Shift+Insert` to the regular clipboard; and the script notes that Konsole supports the fallback by default.
- Pass: automated tests verify all three idempotent configuration files without requiring a live graphical session; a hardware run verifies that the chord produces `F24` without forwarded Ctrl or Alt modifiers.

### AT-14: Reliable low-delay ydotool output
- Setup: install and start the per-user `ydotoold` service.
- Command: start Soundvibes with `[output] mode = "ydotool"`.
- Action: dictate text into a focused graphical text field.
- Expect: Soundvibes sends the transcript through the existing daemon with a 3 ms key delay and 3 ms key hold, without using the KDE-incompatible `wtype` path or the slower `dotool` fallback.
- Pass: automated test-support verifies the exact daemon-client command and stdin payload; a hardware acceptance run verifies that a focused application receives the complete text.

### AT-15: Opt-in transcript debug logging
- Setup: set `debug_transcripts = true` or pass `--debug-transcripts`.
- Command: start Soundvibes and complete an utterance containing quotes or a newline.
- Expect: Soundvibes writes the exact transcript to stderr as one escaped debug line before sending it to the configured output backend.
- Pass: automated test-support verifies the exact escaped log line; debug logging remains disabled by default.

### AT-16: Phonon-2 configuration
- Setup: choose `engine = "phonon2"` or `--engine phonon2`.
- Action: request a non-English language or a sample rate other than 16000 Hz.
- Expect: exit code 2 and a specific configuration error before model loading or microphone access.
- Automated: `at16_phonon2_rejects_unsupported_audio_configuration` in `tests/acceptance.rs`.
- Configuration precedence is also tested by `reads_engine_and_library_directory_with_cli_precedence` in `src/main.rs`.

### AT-16a: Phonon-2 asset setup
- Setup: choose Phonon-2 through TOML and point `--model` at an empty directory.
- Action: start with `engine = "phonon2"`.
- Expect: exit code 2 with the Phonon-2 setup command; no automatic download is attempted.
- Automated: `at16a_phonon2_missing_assets_report_setup`.

### AT-16b: Real Phonon-2 transcription through the daemon
- Setup: run `scripts/prepare-phonon2.py` and set `PHONON2_MODEL_DIR` and `PHONON2_LIBRARY_DIR` to the prepared assets.
- Command: `cargo test --release --test acceptance --features test-support at16b -- --nocapture`.
- Action: replay the bundled 16 kHz mono JFK recording through the daemon's audio adapter, real model factory and background transcription worker.
- Expect: output contains both known "ask not" and "ask what" phrases. Inference uses no Python process or network.
- Automated: `at16b_phonon2_real_model_transcribes_through_daemon_worker`. Missing environment variables skip; configured assets must load successfully.
- Crate-level tests additionally compare the official CPU feature fixture, reject invalid containers, and exercise repeated decode, model reload and worker-thread migration. `PHONON2_REFERENCE_JSON` and `PHONON2_REFERENCE_FEATURES` enable full Python-reference comparison.

### AT-17: Live vocabulary files from multiple applications
- Setup: configure `vocabulary_dir` or `--vocabulary-dir`; by default use `$XDG_CONFIG_HOME/soundvibes/vocabulary.d` or `~/.config/soundvibes/vocabulary.d`.
- Action: while the same daemon runs, create the directory, add and edit per-app `.txt` files, atomically replace a file, delete a file, and delete the directory.
- Expect: the next transcription uses the current combined list. Filename and line order determine priority; deduplication ignores case and retains the first spelling. Whitespace, blank lines and comment lines are handled; `C#` remains a term. Hidden files, temporary files and subdirectories do not contribute terms.
- Automated: `at17_vocabulary_files_reload_and_merge_without_restarting_daemon` in `tests/acceptance.rs` with `test-support`. No hardware or model files required.
- CLI/TOML precedence and the XDG default are covered by `vocabulary_directory_defaults_and_cli_override_toml` in `src/main.rs`.

### AT-17a: Vocabulary errors preserve dictation
- Action: after loading a valid list, introduce invalid UTF-8 or NUL bytes, or replace the directory with a file. Then restore a valid directory and list.
- Expect: each failed read warns and preserves the entire previous list, including when earlier files in a scan changed. Dictation continues; the next successful scan applies the current files.
- Automated: `at17a_bad_vocabulary_updates_keep_last_good_list_and_recover` with `test-support`.

### AT-17b: Real engine vocabulary and capacity
- Action: transcribe the bundled JFK audio with vocabulary, then clear it. Supply an oversized list.
- Expect: Parakeet accepts native vocabulary in offline and streaming runs. Phonon-2 uses native bias, clearing restores the baseline, and a 26-term directory reports that the first 25 terms are used.
- Automated: `at16b_phonon2_real_model_transcribes_through_daemon_worker` in `tests/acceptance.rs` verifies directory-to-engine delivery and its capacity warning. `transcribes_real_audio_without_python` in `crates/phonon2/tests/transcription.rs` verifies native bias, clearing and zero strength.
- Missing external model inputs skip ordinary runs. Configure `SV_PARAKEET_MODEL`, `PHONON2_MODEL_DIR` and `PHONON2_LIBRARY_DIR` to exercise real engines. The Phonon-2 libraries must be the pinned 0.2.11 version. Live microphone and Arm hardware are outside these automated checks.

### AT-38: Parakeet Unified EN offline recognition

`parakeet_real_asset_offline` transcribes the JFK fixture through the native Parakeet backend with vocabulary terms. It first cancels a live
session, then checks offline reuse of that session.
Set `SV_PARAKEET_MODEL` to the verified Q8_0 GGUF to enable it; unset skips.
An explicitly configured missing/invalid asset fails. `SV_PARAKEET_BACKEND=cpu`
is the default for this test; set `vulkan` to exercise the GPU.

### AT-39: Live Parakeet recording and release

`parakeet_real_asset_streams_through_daemon` runs the real Parakeet backend through
capture, worker and daemon final output with vocabulary enabled and continuous VAD.
It feeds an initial chunk during recording and the remaining JFK audio on release,
and rejects streaming fallback warnings. Run
`SV_PARAKEET_MODEL=PATH cargo test --test acceptance --features test-support parakeet`.
Set `SV_PARAKEET_BACKEND=vulkan` for GPU validation. Unset model skips cleanly.
Worker unit tests additionally cover FIFO result order, cancellation, unavailable
streaming, and begin/feed/finish failures with complete-audio offline fallback.

### AT-40: Continuous output with live Parakeet

- `at40_live_stream_outputs_on_pause_before_release` and
  `at40_live_stream_outputs_during_long_speech` require finalized streaming output
  before sending key release. Their engine adapter rejects offline fallback.
- `at40_live_timed_overlap_is_fed_and_deduped_on_release` verifies exact audio
  delivery across timed cuts and removes repeated words from the release tail.
- `at40_real_parakeet_outputs_on_pause_and_duration_before_release` uses the JFK
  fixture and actual model for both cut reasons, rejects fallback/errors, and
  waits for native transcript output before releasing. Set `SV_PARAKEET_MODEL`
  and optionally `SV_PARAKEET_BACKEND=vulkan`; unset model skips gracefully.
- `at40_live_pause_after_output_does_not_cancel_the_hold` uses a nonzero
  no-speech timeout, pauses after finalized output, resumes speech without key
  release, and requires a second segment.
- Run `cargo test --test acceptance --features test-support at40`.

### AT-41: KDE system tray listening state and recent transcripts

- Starting the daemon registers a KDE StatusNotifierItem. Clicking its icon opens the transcript menu.
- The icon and tooltip show Listening only while microphone capture is active. Release, cancellation, and shutdown end that state; failed capture must not enable it.
- The popup shows the last ten nonempty completed transcripts, newest first, regardless of the output destination. Preserve the exact text, including Unicode and underscores.
- Clicking a transcript copies its full, unescaped text to the clipboard, with no paste or clipboard restoration. History copies wait for any active dictation paste/restore transaction.
- Show "No transcripts yet" before the first transcript. History stays in memory and resets when the daemon restarts.
- A missing desktop session must not prevent dictation. The tray reconnects if Plasma's watcher restarts.
- Automated: `tray_tracks_capture_and_recent_transcripts`, `tray_history_keeps_the_latest_ten_transcripts`, `tray_click_copies_the_selected_transcript`, `tray_copy_waits_for_dictation_clipboard_transaction`, `tray_stays_idle_after_capture_failure`, `tray_returns_to_idle_after_silence_cancellation` in `tests/acceptance.rs`.
- Desktop check: start `sv daemon start` in KDE Plasma, click the icon before and after dictating, hold/release the recording hotkey, then stop the daemon. Confirm both icons, tooltip, popup ordering, and removal on exit. This needs a running Plasma session.
