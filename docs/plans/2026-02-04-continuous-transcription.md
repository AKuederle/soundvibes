# Continuous Transcription with VAD Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Enable per-pause transcription during recording using whisper.cpp's native Silero VAD support.

**Architecture:** Replace the current simple energy-based VAD with whisper.cpp's Silero VAD model. During recording, monitor audio for speech segments. When a segment ends (silence detected), immediately transcribe and inject text while continuing to record. This enables continuous dictation without needing to toggle off.

**Tech Stack:** whisper.cpp Silero VAD (ggml-silero-v6.2.0.bin), Rust FFI bindings

---

## Background

### Current Behavior
- Toggle ON → record to buffer
- Toggle OFF → transcribe entire buffer → inject text
- VAD only trims trailing silence, doesn't trigger transcription

### New Behavior
- Toggle ON → start recording with VAD monitoring
- Pause detected → transcribe segment → inject text → continue recording
- Toggle OFF → transcribe any remaining audio → stop

### whisper.cpp Native VAD Features
- `whisper_vad_context` - Silero VAD model context
- `whisper_vad_segments_from_samples` - detect speech segments in audio
- `whisper_vad_params`:
  - `threshold` - speech probability threshold (default ~0.5)
  - `min_speech_duration_ms` - minimum valid speech (default 250ms)
  - `min_silence_duration_ms` - silence to end segment (default 2000ms)
  - `speech_pad_ms` - padding around segments (default 400ms)

---

## Task 1: Add VAD Model Download Support

**Files:**
- Modify: `src/model.rs`
- Modify: `install.sh`

**Step 1: Add VAD model constants to model.rs**

After the whisper model constants, add:

```rust
const VAD_MODEL_NAME: &str = "ggml-silero-v6.2.0.bin";
const VAD_MODEL_URL: &str = "https://huggingface.co/ggml-org/whisper-vad/resolve/main/ggml-silero-v6.2.0.bin";
const VAD_MODEL_SIZE: u64 = 2_000_000; // ~2MB
```

**Step 2: Add VAD model path function**

```rust
pub fn vad_model_path() -> Option<PathBuf> {
    data_dir().map(|dir| dir.join("models").join(VAD_MODEL_NAME))
}

pub fn ensure_vad_model() -> Result<PathBuf, AppError> {
    let path = vad_model_path()
        .ok_or_else(|| AppError::config("cannot determine data directory"))?;

    if path.exists() {
        return Ok(path);
    }

    download_model(VAD_MODEL_URL, &path, VAD_MODEL_SIZE)?;
    Ok(path)
}
```

**Step 3: Verify it compiles**

Run: `cargo check`
Expected: Success

**Step 4: Commit**

```bash
git add src/model.rs
git commit -m "feat: add VAD model download support"
```

---

## Task 2: Generate VAD Bindings

**Files:**
- Modify: `build.rs`

**Step 1: Add VAD functions to bindgen whitelist**

In `build.rs`, find the bindgen builder and add VAD functions:

```rust
.allowlist_function("whisper_vad_.*")
.allowlist_type("whisper_vad_.*")
```

**Step 2: Rebuild to generate bindings**

Run: `cargo build`
Expected: New VAD bindings generated in whisper_bindings.rs

**Step 3: Verify VAD bindings exist**

Run: `grep "whisper_vad" target/debug/build/sv-*/out/whisper_bindings.rs | head -5`
Expected: Shows whisper_vad functions

**Step 4: Commit**

```bash
git add build.rs
git commit -m "feat: add VAD function bindings"
```

---

## Task 3: Create VadContext Wrapper

**Files:**
- Create: `src/vad.rs`
- Modify: `src/lib.rs`

**Step 1: Create vad.rs with VadContext struct**

```rust
use std::ffi::CString;
use std::path::Path;
use std::ptr::NonNull;

use crate::whisper::bindings::*;

#[derive(Debug)]
pub enum VadError {
    InitFailed,
    InvalidPath,
}

impl std::fmt::Display for VadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VadError::InitFailed => write!(f, "failed to initialize VAD context"),
            VadError::InvalidPath => write!(f, "invalid VAD model path"),
        }
    }
}

pub struct VadContext {
    ctx: NonNull<whisper_vad_context>,
}

impl VadContext {
    pub fn from_file(path: &Path) -> Result<Self, VadError> {
        let path_c = CString::new(path.to_str().ok_or(VadError::InvalidPath)?)
            .map_err(|_| VadError::InvalidPath)?;

        let params = unsafe { whisper_vad_default_context_params() };
        let ctx = unsafe { whisper_vad_init_from_file_with_params(path_c.as_ptr(), params) };
        let ctx = NonNull::new(ctx).ok_or(VadError::InitFailed)?;

        Ok(Self { ctx })
    }

    /// Detect speech segments in audio samples.
    /// Returns Vec of (start_sec, end_sec) tuples.
    pub fn detect_segments(
        &self,
        samples: &[f32],
        sample_rate: u32,
        min_silence_ms: u32,
    ) -> Vec<(f32, f32)> {
        let mut params = unsafe { whisper_vad_default_params() };
        params.min_silence_duration_ms = min_silence_ms as i32;

        let segments = unsafe {
            whisper_vad_segments_from_samples(
                self.ctx.as_ptr(),
                samples.as_ptr(),
                samples.len() as i32,
                sample_rate as i32,
                params,
            )
        };

        if segments.is_null() {
            return Vec::new();
        }

        let n = unsafe { whisper_vad_segments_n_segments(segments) };
        let mut result = Vec::with_capacity(n as usize);

        for i in 0..n {
            let t0 = unsafe { whisper_vad_segments_get_segment_t0(segments, i) };
            let t1 = unsafe { whisper_vad_segments_get_segment_t1(segments, i) };
            result.push((t0, t1));
        }

        unsafe { whisper_vad_free_segments(segments) };
        result
    }
}

impl Drop for VadContext {
    fn drop(&mut self) {
        unsafe { whisper_vad_free(self.ctx.as_ptr()) };
    }
}

// Safety: VadContext is thread-safe for read operations
unsafe impl Send for VadContext {}
unsafe impl Sync for VadContext {}
```

**Step 2: Add module to lib.rs**

Add after other pub mod declarations:

```rust
pub mod vad;
```

**Step 3: Verify it compiles**

Run: `cargo check`
Expected: Success (may have warnings about unused code)

**Step 4: Commit**

```bash
git add src/vad.rs src/lib.rs
git commit -m "feat: add VadContext wrapper for whisper.cpp VAD"
```

---

## Task 4: Add Continuous Mode to DaemonConfig

**Files:**
- Modify: `src/types.rs`
- Modify: `src/daemon.rs`
- Modify: `src/main.rs`

**Step 1: Add VadMode::Continuous variant**

In `src/types.rs`, modify VadMode enum:

```rust
#[derive(Debug, Copy, Clone, PartialEq, Eq, ValueEnum, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VadMode {
    On,
    Off,
    Continuous,
}
```

**Step 2: Add vad_model_path to DaemonConfig**

In `src/daemon.rs`, add to DaemonConfig struct:

```rust
pub struct DaemonConfig {
    // ... existing fields ...
    pub vad_model_path: Option<PathBuf>,
}
```

**Step 3: Update Config struct in main.rs**

Add field and pass through from CLI/config file (similar to other optional paths).

**Step 4: Update daemon_config construction in main.rs**

Pass `vad_model_path: None` for now (will be populated in next task).

**Step 5: Update test DaemonConfig in daemon.rs**

Add `vad_model_path: None` to test config.

**Step 6: Verify it compiles**

Run: `cargo check`
Expected: Success

**Step 7: Commit**

```bash
git add src/types.rs src/daemon.rs src/main.rs
git commit -m "feat: add VadMode::Continuous and vad_model_path config"
```

---

## Task 5: Implement Continuous Recording Loop

**Files:**
- Modify: `src/daemon.rs`

**Step 1: Add VAD context initialization**

In `run_daemon_loop`, after transcriber initialization:

```rust
let vad_ctx = if config.vad == VadMode::Continuous {
    let vad_path = config.vad_model_path.as_ref()
        .ok_or_else(|| AppError::config("VAD model path required for continuous mode"))?;
    Some(vad::VadContext::from_file(vad_path)
        .map_err(|e| AppError::runtime(e.to_string()))?)
} else {
    None
};
```

**Step 2: Add continuous mode processing in recording loop**

Replace the simple buffer drain with segment detection:

```rust
if recording {
    if let Some(active) = capture.as_mut() {
        active.drain(&mut buffer);

        // In continuous mode, check for completed segments
        if let Some(ref vad) = vad_ctx {
            let segments = vad.detect_segments(
                &buffer,
                config.sample_rate,
                config.vad_silence_ms as u32,
            );

            // If we have a completed segment (not the last one which may be ongoing)
            if segments.len() > 1 {
                let (start, end) = segments[0];
                let start_sample = (start * config.sample_rate as f32) as usize;
                let end_sample = (end * config.sample_rate as f32) as usize;

                // Extract and transcribe the completed segment
                let segment_samples: Vec<f32> = buffer[start_sample..end_sample].to_vec();

                utterance_index += 1;
                let transcript = transcriber
                    .transcribe(&segment_samples, Some(&config.language))
                    .map_err(|err| AppError::runtime(err.to_string()))?;

                emit_transcript(config, output, &transcript, audio::SegmentInfo {
                    index: utterance_index,
                    duration_ms: ((end - start) * 1000.0) as u64,
                })?;

                // Remove processed samples, keep from second segment onward
                let keep_from = (segments[1].0 * config.sample_rate as f32) as usize;
                buffer.drain(..keep_from.min(buffer.len()));
            }
        }
    }
}
```

**Step 3: Verify it compiles**

Run: `cargo check`
Expected: Success

**Step 4: Run tests**

Run: `cargo test`
Expected: All tests pass

**Step 5: Commit**

```bash
git add src/daemon.rs
git commit -m "feat: implement continuous transcription with VAD segment detection"
```

---

## Task 6: Auto-download VAD Model

**Files:**
- Modify: `src/main.rs`

**Step 1: Download VAD model when continuous mode enabled**

In main.rs, after model preparation, add:

```rust
let vad_model_path = if config.vad == VadMode::Continuous {
    output.stdout("Downloading VAD model if needed...");
    Some(model::ensure_vad_model()?)
} else {
    None
};
```

**Step 2: Pass to daemon config**

Update DaemonConfig construction to use `vad_model_path`.

**Step 3: Verify it compiles**

Run: `cargo check`
Expected: Success

**Step 4: Commit**

```bash
git add src/main.rs
git commit -m "feat: auto-download VAD model for continuous mode"
```

---

## Task 7: Add Integration Test

**Files:**
- Create: `tests/continuous_mode.rs`

**Step 1: Write integration test**

```rust
//! Integration test for continuous transcription mode

use std::time::Duration;

#[test]
#[ignore] // Requires VAD model and audio device
fn continuous_mode_transcribes_on_pause() {
    // This test would need audio fixtures and VAD model
    // Manual testing recommended for now
}
```

**Step 2: Update documentation**

Add to README.md under Output Modes:

```markdown
### Continuous Mode (--vad continuous)

Transcribes and injects text after each pause, without needing to toggle off:

```bash
sv daemon start --vad continuous
sv  # toggle on, speak naturally with pauses, text appears after each pause
sv  # toggle off when done
```

This mode uses whisper.cpp's Silero VAD model (~2MB, auto-downloaded on first use).
```

**Step 3: Commit**

```bash
git add tests/continuous_mode.rs README.md
git commit -m "docs: add continuous mode documentation and test stub"
```

---

## Task 8: Manual Testing

**Steps:**

1. Build release binary:
   ```bash
   cargo build --release
   cp target/release/sv ~/.local/bin/sv
   ```

2. Start daemon in continuous mode:
   ```bash
   sv daemon start --vad continuous --mode stdout
   ```

3. In another terminal, toggle recording:
   ```bash
   sv
   ```

4. Speak a sentence, pause for 2 seconds, speak another sentence.

5. Verify:
   - First transcript appears after first pause
   - Second transcript appears after second pause
   - Both without toggling off

6. Toggle off:
   ```bash
   sv
   ```

---

## Summary

| Task | Description | Estimated Effort |
|------|-------------|-----------------|
| 1 | VAD model download support | Small |
| 2 | Generate VAD bindings | Small |
| 3 | VadContext wrapper | Medium |
| 4 | DaemonConfig changes | Small |
| 5 | Continuous recording loop | Large |
| 6 | Auto-download VAD model | Small |
| 7 | Tests and docs | Small |
| 8 | Manual testing | Manual |

## References

- [whisper.cpp VAD support](https://github.com/ggml-org/whisper.cpp)
- [Silero VAD models on HuggingFace](https://huggingface.co/ggml-org/whisper-vad)
- [whisper.cpp stream example](https://github.com/ggml-org/whisper.cpp/blob/master/examples/stream/README.md)
