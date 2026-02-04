# Audio Feedback Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add audio feedback sounds when recording starts/stops for better UX.

**Architecture:** Use `paplay` to play freedesktop system sounds. Sounds play asynchronously (non-blocking). Configurable via CLI flag `--audio-feedback`.

**Tech Stack:** paplay (PulseAudio/PipeWire), freedesktop sound theme

---

## Sound Mapping

| Event | Sound | Path |
|-------|-------|------|
| Recording start | device-added | `/usr/share/sounds/freedesktop/stereo/device-added.oga` |
| Recording stop | complete | `/usr/share/sounds/freedesktop/stereo/complete.oga` |

---

## Task 1: Add Audio Feedback Module

**Files:**
- Create: `src/feedback.rs`
- Modify: `src/lib.rs`

**Step 1: Create feedback.rs**

```rust
//! Audio feedback for recording state changes.

use std::process::Command;

const SOUND_START: &str = "/usr/share/sounds/freedesktop/stereo/device-added.oga";
const SOUND_STOP: &str = "/usr/share/sounds/freedesktop/stereo/complete.oga";

pub fn play_start_sound() {
    play_sound(SOUND_START);
}

pub fn play_stop_sound() {
    play_sound(SOUND_STOP);
}

fn play_sound(path: &str) {
    // Spawn paplay in background, ignore errors (sound is optional)
    let _ = Command::new("paplay")
        .arg(path)
        .spawn();
}
```

**Step 2: Add module to lib.rs**

```rust
pub mod feedback;
```

**Step 3: Verify it compiles**

Run: `cargo check`

**Step 4: Commit**

```bash
git add src/feedback.rs src/lib.rs
git commit -m "feat: add audio feedback module"
```

---

## Task 2: Add CLI Flag

**Files:**
- Modify: `src/main.rs`
- Modify: `src/types.rs`
- Modify: `src/daemon.rs`

**Step 1: Add flag to Cli struct in main.rs**

After the `dump_audio` flag:

```rust
#[arg(long, default_value_t = false, global = true)]
audio_feedback: bool,
```

**Step 2: Add to Config struct in main.rs**

```rust
audio_feedback: bool,
```

**Step 3: Add to Config::from_sources**

```rust
let audio_feedback = if matches.value_source("audio_feedback") == Some(ValueSource::CommandLine) {
    cli.audio_feedback
} else {
    file.audio_feedback.unwrap_or(cli.audio_feedback)
};
```

And include in the returned Config.

**Step 4: Add to FileConfig in main.rs**

```rust
audio_feedback: Option<bool>,
```

**Step 5: Add to DaemonConfig in daemon.rs**

```rust
pub audio_feedback: bool,
```

**Step 6: Pass through in main.rs daemon_config construction**

```rust
audio_feedback: config.audio_feedback,
```

**Step 7: Update test config in daemon.rs**

Add `audio_feedback: false` to test DaemonConfig.

**Step 8: Verify and commit**

```bash
cargo check
cargo test
git add src/main.rs src/daemon.rs
git commit -m "feat: add --audio-feedback CLI flag"
```

---

## Task 3: Play Sounds on Toggle

**Files:**
- Modify: `src/daemon.rs`

**Step 1: Add import**

```rust
use crate::feedback;
```

**Step 2: Play sound on toggle on**

In the Toggle handler, after `output.stdout("Toggle on. Recording...");`:

```rust
if config.audio_feedback {
    feedback::play_start_sound();
}
```

**Step 3: Play sound on toggle off**

In stop_recording, after draining the capture, before finalize_recording:

```rust
if config.audio_feedback {
    feedback::play_stop_sound();
}
```

**Step 4: Verify and commit**

```bash
cargo check
cargo test
git add src/daemon.rs
git commit -m "feat: play audio feedback on recording toggle"
```

---

## Task 4: Update Service and Documentation

**Files:**
- Modify: `contrib/sv.service`
- Modify: `README.md`

**Step 1: Enable in service file**

```ini
ExecStart=%h/.local/bin/sv daemon start --vad continuous --vad-silence-ms 600 --audio-feedback
```

**Step 2: Add to README**

In Quick Tips section:

```markdown
**Audio Feedback:**
```bash
sv daemon start --audio-feedback
```
Plays a sound when recording starts/stops. Requires PulseAudio/PipeWire.
```

**Step 3: Commit**

```bash
git add contrib/sv.service README.md
git commit -m "docs: add audio feedback documentation"
```

---

## Task 5: Manual Testing

1. Rebuild and install: `cargo build --release && cp target/release/sv ~/.local/bin/`
2. Reload service: `systemctl --user daemon-reload && systemctl --user restart sv.service`
3. Toggle recording: `sv`
4. Verify start sound plays
5. Toggle off: `sv`
6. Verify stop sound plays

---

## Summary

| Task | Description |
|------|-------------|
| 1 | Add feedback module with paplay |
| 2 | Add --audio-feedback CLI flag |
| 3 | Play sounds on toggle |
| 4 | Update service and docs |
| 5 | Manual testing |
