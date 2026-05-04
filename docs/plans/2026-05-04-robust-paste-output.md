# Robust Paste Output With Clipboard Restore

## Overview

Replace SoundVibes' current text injection path with a paste-oriented output system inspired by VoxType, but adapted for KDE clipboard behavior. The new path should treat clipboard use as a short-lived transport mechanism: save the existing clipboard, copy the transcription, trigger paste, then restore or clear the clipboard so transcribed text does not remain as the active clipboard item.

## Problem

The current implementation in `src/output.rs` mixes several concerns:

1. Auto mode tries clipboard paste before direct typing.
2. Clipboard paste currently depends on key simulation paths that have been unreliable on KDE.
3. Clipboard save/restore stores only raw bytes, then restores as `text/plain`.
4. KDE/Klipper can keep the transcribed text in clipboard history, which is annoying for repeated dictation.
5. Terminal-specific paste handling is hard-coded through focused-window detection.

The main user-visible issue is that temporary transcription text can stay in the clipboard after paste. A robust implementation should preserve the previous clipboard content, including its MIME type, and should avoid polluting KDE clipboard history when possible.

## Goals

- Make paste mode the primary robust output path for Wayland/KDE.
- Preserve and restore clipboard content with MIME type metadata.
- Clear the clipboard when it was empty before paste.
- Keep KDE/Klipper history suppression for temporary transcription copies.
- Use configurable paste keys instead of terminal-class detection.
- Separate output modes so type, clipboard-only, and paste behavior are explicit.
- Keep fallbacks simple and diagnosable.

## Non-Goals

- Preserve the current `try_clipboard_paste` implementation.
- Keep terminal-window class detection as the default paste shortcut mechanism.
- Implement a native Wayland/libei client inside SoundVibes in this change.
- Guarantee restoration of every possible multi-MIME clipboard payload in the first version.

## Proposed Output Model

Add explicit output modes:

- `type`: direct typing through configured drivers.
- `paste`: copy transcription to clipboard, trigger paste, then restore previous clipboard.
- `clipboard`: copy transcription and leave it for manual paste.

The immediate migration should focus on `paste`. The existing direct typing backends can remain available during the transition, but the old auto path should no longer silently prefer clipboard paste before every other backend.

## Paste Mode Flow

1. Read the current clipboard.
2. Copy transcribed text to the clipboard with a KDE history-suppression MIME hint.
3. Wait for the compositor/clipboard manager to observe the new clipboard content.
4. Send the configured paste keystroke.
5. Wait for the focused application to consume the paste.
6. Restore the previous clipboard content with its original MIME type, or clear the clipboard if it was originally empty.

Default timings:

- `pre_paste_delay_ms = 100`
- `restore_clipboard_delay_ms = 250`

Both values should be configurable because Electron apps, terminals, and remote desktop sessions can consume clipboard data at different speeds.

## Clipboard Capture

Represent saved clipboard data explicitly:

```rust
struct ClipboardSnapshot {
    mime_type: String,
    data: Vec<u8>,
}
```

Capture behavior:

1. Prefer Wayland clipboard commands when `WAYLAND_DISPLAY` is set.
2. Run `wl-paste --list-types`.
3. If no types are available, treat the clipboard as empty.
4. Pick the first MIME type for the first implementation.
5. Run `wl-paste --type <mime_type>` and store bytes plus MIME type.
6. Refuse to save very large clipboard contents; use a limit such as `100 MiB`.

X11 fallback can be added later with `xclip -selection clipboard -o`, but it cannot preserve rich MIME metadata as well as `wl-paste`. For this KDE-focused change, Wayland should be the acceptance target.

## Clipboard Restore

Restore behavior:

1. If a snapshot exists, run `wl-copy --type <mime_type>` and write the saved bytes to stdin.
2. If no snapshot exists, clear the clipboard.
3. Log restore failures but do not report the paste as failed if the text was already pasted.

For KDE/Klipper, the temporary transcription copy should include:

- `text/plain` containing the transcription.
- `x-kde-passwordManagerHint` containing a small marker value, for example `secret`.

When restoring the original clipboard, do not blindly apply the KDE secret marker unless we intentionally want to hide the restored item from history. The restore path should preserve the user's clipboard semantics as much as possible.

## Paste Keystroke

Replace terminal detection with a configurable paste shortcut:

- default: `ctrl+v`
- common alternatives: `ctrl+shift+v`, `shift+insert`

Add a parser for strings like:

- `ctrl+v`
- `ctrl+shift+v`
- `shift+insert`

The parsed keystroke should be convertible to each available key simulation tool.

Initial keystroke driver:

1. `wtype` on Wayland when installed.

Optional later driver:

2. `eitype` for KDE/GNOME libei support if the project wants to add it as a runtime dependency option.

## Configuration

Add or evolve configuration fields:

```toml
[output]
mode = "paste"
paste_keys = "ctrl+v"
restore_clipboard = true
pre_paste_delay_ms = 100
restore_clipboard_delay_ms = 250
```

Recommended defaults for KDE:

```toml
[output]
mode = "paste"
restore_clipboard = true
paste_keys = "ctrl+v"
```

If terminal use is common, users can set:

```toml
paste_keys = "ctrl+shift+v"
```

## Implementation Steps

### 1. Introduce Clipboard Snapshot Types

File: `src/output.rs` or a new `src/output/clipboard.rs`

- Add `ClipboardSnapshot`.
- Add `read_clipboard_snapshot() -> Result<Option<ClipboardSnapshot>, OutputError>`.
- Add `restore_clipboard_snapshot(snapshot: Option<&ClipboardSnapshot>)`.
- Add size limits and clear error messages.

### 2. Replace Clipboard Copy Logic

File: `src/output.rs` or a new `src/output/paste.rs`

- Replace `clipboard_copy_secret()` with a command-backed copy implementation or a `wl-clipboard-rs` implementation that can preserve MIME behavior.
- Keep KDE secret MIME only for the temporary transcription.
- Avoid restoring all content as `text/plain`.

### 3. Add Paste Keystroke Parser

File: `src/output.rs` or a new `src/output/keys.rs`

- Parse `paste_keys`.
- Convert parsed keys to `wtype` args.
- Return explicit errors for unknown key names.

### 4. Implement Paste Output

File: `src/output.rs` or a new `src/output/paste.rs`

- Save clipboard if `restore_clipboard` is enabled.
- Copy transcription with KDE hint.
- Sleep for `pre_paste_delay_ms`.
- Send paste keystroke using configured drivers.
- Sleep for `restore_clipboard_delay_ms`.
- Restore snapshot or clear clipboard.

### 5. Update CLI and Config

Files: `src/types.rs`, `src/main.rs`, `src/daemon.rs`

- Add explicit output/injection mode selection if needed.
- Add flags/config for paste keys and restore delays.
- Remove the old `--inject-backend` codepath and route output through explicit modes.

### 6. Update Documentation

Files: `README.md`, `docs/acceptance-tests.md`

- Document paste mode as recommended for KDE Wayland.
- Explain clipboard restore behavior.
- Document `paste_keys` examples.
- Add acceptance criteria for clipboard restoration.

## Testing Plan

Unit tests:

- Parse `ctrl+v`, `ctrl+shift+v`, and `shift+insert`.
- Reject malformed paste key strings.
- Convert paste keys to expected `wtype` argument sequences.
- Preserve MIME type in `ClipboardSnapshot`.
- Refuse oversized clipboard snapshots.

Integration-style tests with test doubles:

- Existing clipboard snapshot is restored after paste.
- Empty original clipboard is cleared after paste.
- Paste keystroke failure still attempts clipboard restoration.
- Restore failure is surfaced as a warning but does not hide a successful paste.

Acceptance test mapping:

- Add an entry to `docs/acceptance-tests.md`.
- Add a deterministic test in `tests/acceptance.rs` using mock command runners or test-support hooks.
- Hardware/manual acceptance for KDE Wayland:
  1. Put known text in clipboard.
  2. Trigger SoundVibes paste output into a text field.
  3. Verify pasted transcription appears.
  4. Verify clipboard returns to the original text.
  5. Verify Klipper history does not retain the temporary transcription when the KDE hint is honored.

Validation commands:

```bash
cargo fmt --check
cargo test --test acceptance --features test-support
cargo test
```

If `mise` is available:

```bash
mise run ci
```

## Risks and Tradeoffs

- Clipboard managers may observe temporary clipboard data despite the KDE secret MIME hint.
- Some applications consume clipboard data slowly; restore delay must remain configurable.
- Preserving only the first MIME type may not fully restore complex clipboard contents such as file lists or images with multiple formats.
- `wtype` depends on compositor protocol support; KDE environments that reject `wtype` will still need manual clipboard paste or a future libei/eitype path.
- Clearing/restoring clipboard after paste may surprise users who expect transcription text to remain available; this should be explicit in docs and config.

## Open Questions

- Should `restore_clipboard = true` be the default only for `paste`, or globally for all clipboard-assisted modes?
- Should SoundVibes eventually add `eitype` as a paste keystroke driver for KDE/GNOME?
- Do we want full multi-MIME clipboard preservation, or is first-MIME preservation enough for the first version?
- Should the KDE secret hint also be used when restoring the original clipboard, or only for temporary transcription copies?
