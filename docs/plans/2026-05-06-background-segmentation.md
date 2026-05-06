# Background Segmentation and Transcription Plan

## Context

SoundVibes currently handles continuous mode synchronously inside `run_daemon_loop`:

- Audio is drained into one active buffer.
- A silence boundary immediately calls `transcriber.transcribe(...)`.
- The daemon loop emits output before it returns to draining audio.

That means audio capture callbacks may continue while transcription runs, but SoundVibes is not draining its capture buffer during inference. With `large-v3-turbo`, synchronous transcription can take long enough to risk dropped or delayed audio during long dictation.

The requested behavior is pause-first segmentation with a bounded maximum delay:

- If the user pauses naturally, transcribe at the pause.
- If the user keeps talking, start transcription roughly every N seconds.
- Avoid hard cutting exactly at N seconds when a natural low-energy boundary is nearby.

## VoxType Reference

VoxType implements this as "eager processing":

- Config:
  - `eager_processing = false`
  - `eager_chunk_secs = 5.0`
  - `eager_overlap_secs = 0.5`
- It accumulates audio while recording.
- `count_complete_chunks(...)` checks how many fixed-size chunks are ready.
- `extract_chunk(...)` extracts chunk `i` using `start = i * (chunk - overlap)`.
- Each chunk is transcribed in a background task:
  - `tokio::task::spawn_blocking(move || transcriber.transcribe(&chunk_audio))`
- Completed chunk tasks are polled while recording continues.
- On stop, VoxType waits for remaining chunk tasks, transcribes the tail, then combines results.
- Chunk result combination sorts by `chunk_index` and removes duplicated words at overlap boundaries by matching the longest previous suffix against the next prefix.

Important takeaways for SoundVibes:

- Keep audio draining independent from inference.
- Preserve output order with chunk indexes.
- Use overlap plus boundary deduplication to reduce dropped/duplicated words.
- Transcribe the final tail on stop.

## Product Decision

Do not add a generic multi-driver or multi-mode system. Add one SoundVibes behavior:

**Continuous background segmentation.**

When `vad = "continuous"`, segmentation should be:

1. Pause-first.
2. Time-bounded.
3. Background-transcribed.

Suggested initial defaults:

- `segment_target_ms = 10_000`
- `segment_grace_ms = 2_000`
- `segment_overlap_ms = 400`
- `segment_min_ms = 1_200`

Behavior:

- Before `segment_target_ms`, cut only on normal silence (`vad_silence_ms`).
- After `segment_target_ms`, enter a grace window.
- During the grace window, cut at the first low-energy chunk.
- If no low-energy chunk appears by `segment_target_ms + segment_grace_ms`, force a cut.
- Include `segment_overlap_ms` of previous audio in the next segment.
- Continue draining audio while earlier segments are transcribing.

## Proposed Architecture

### 1. Add Segmentation Config

Add to `DaemonConfig`:

- `segment_target_ms: u64`
- `segment_grace_ms: u64`
- `segment_overlap_ms: u64`
- `segment_min_ms: u64`

CLI flags:

- `--segment-target-ms`
- `--segment-grace-ms`
- `--segment-overlap-ms`
- `--segment-min-ms`

Defaults should make the feature active only in continuous mode. For non-continuous VAD, existing start/stop behavior should remain unchanged.

### 2. Extract Segment Boundary Logic

Create a small module, likely `src/segmentation.rs`, with deterministic tests.

Core types:

```rust
pub struct SegmentConfig {
    pub sample_rate: u32,
    pub vad_threshold: f32,
    pub silence_samples: usize,
    pub target_samples: usize,
    pub grace_samples: usize,
    pub overlap_samples: usize,
    pub min_segment_samples: usize,
}

pub enum SegmentDecision {
    Continue,
    Cut { speech_end: usize, reason: CutReason },
}

pub enum CutReason {
    Silence,
    SoftLimitPause,
    HardLimit,
}
```

Tests:

- Cuts on normal silence before target.
- Does not cut before `segment_min_ms`.
- After target, cuts on the first low-energy chunk.
- If no pause appears in grace window, cuts at hard limit.
- Carries overlap samples into the next segment.

### 3. Add a Transcription Worker

Use standard threads and `std::sync::mpsc` to match the current codebase.

Worker input:

```rust
struct TranscriptionJob {
    index: u64,
    samples: Vec<f32>,
    duration_ms: u64,
    language: String,
}
```

Worker output:

```rust
struct TranscriptionResult {
    index: u64,
    duration_ms: u64,
    transcript: Result<String, AppError>,
}
```

Worker ownership:

- Move the loaded transcriber into the worker thread.
- The daemon loop no longer calls `transcriber.transcribe(...)` directly during recording.
- The worker receives one job at a time and sends results back.
- FIFO processing preserves order if there is only one worker.

Why one worker:

- Whisper context/thread-safety stays simple.
- Output order is naturally preserved.
- GPU memory pressure stays bounded.
- This is enough to keep audio draining while inference runs.

### 4. Integrate Worker Into Daemon Loop

Replace synchronous continuous transcription with:

1. Drain audio every loop as today.
2. Run segmentation decision on the active buffer.
3. When a segment is ready:
   - Move/copy segment audio into `TranscriptionJob`.
   - Send the job to the worker.
   - Keep only overlap/tail samples in the active buffer.
   - Reset silence and speech tracking for the next segment.
4. Poll worker results every loop without blocking.
5. Emit transcripts as results arrive.

On key release:

- Drain capture once.
- Segment any remaining non-empty tail.
- Send tail job.
- Mark recording inactive.
- Wait for pending jobs from the current recording before reporting fully ready, or keep the daemon able to start a new recording but preserve output ordering.

Initial recommendation:

- Wait for pending jobs on release before "Ready for next utterance."
- Keep audio safe while key is held, which is the main problem.
- Revisit cross-recording concurrency later if needed.

### 5. Preserve Output Order

With one worker, results are usually ordered. Still keep `index` and a small pending map:

- `next_emit_index`
- `BTreeMap<u64, TranscriptionResult>`

Emit only when `result.index == next_emit_index`.

This protects against a future multi-worker implementation and makes tests clearer.

### 6. Boundary Deduplication

Start simple:

- For pause-first cuts, no dedupe needed if we do not overlap text segments at output.
- For timed cuts with overlap, duplicated words can happen.

Implement VoxType-style word dedupe:

- Compare previous emitted transcript suffix against next transcript prefix.
- Remove longest case-insensitive word overlap.
- Only apply when the current job included overlap.

Tests:

- `"hello world"` + `"world again"` => `"again"`
- `"this is a test"` + `"is a test now"` => `"now"`
- no overlap leaves text unchanged.

### 7. Model Reload and Shutdown

Current `SetModel` can happen while daemon is running. With a worker:

- If recording, stop capture and cancel/flush jobs before reload.
- Stop the worker thread by dropping/sending shutdown.
- Load the new model.
- Start a new worker with the new transcriber.

On shutdown:

- Stop capture.
- Send final tail if appropriate.
- Wait for worker completion for a bounded time.
- Emit any ready results.
- Join worker thread.

### 8. Acceptance Tests

Add acceptance coverage in `tests/acceptance.rs` under `test-support`:

- Long held recording with no silence creates at least two transcription jobs and emits two transcripts.
- Audio keeps draining while the first transcription is blocked.
- A pause before target still transcribes before release.
- Forced segmentation after target still transcribes before release.

To make "audio keeps draining" testable, extend `TestTranscriberFactory` with a blocking transcriber:

- First job blocks on a channel.
- Test continues feeding/draining audio.
- Assert second segment can be queued or active buffer continues growing while first job is blocked.

## Implementation Slices

### Slice 1: Pure Segmentation Module

- Add config types and boundary decision logic.
- Add unit tests for pause-first and soft/hard limit behavior.
- No daemon behavior change.

### Slice 2: Worker Skeleton

- Add worker thread types.
- Move transcription into a worker for final stop only.
- Keep behavior equivalent for non-continuous mode.
- Tests prove output remains ordered.

### Slice 3: Continuous Mode Uses Worker

- Replace synchronous continuous transcription with job submission.
- Poll worker results in the daemon loop.
- Existing continuous acceptance tests must still pass.

### Slice 4: Time-Bounded Segmentation

- Add `segment_target_ms`, `segment_grace_ms`, `segment_overlap_ms`, `segment_min_ms`.
- Cut on pause or soft/hard time limit.
- Add acceptance tests for long monologues.

### Slice 5: Deduplication and Polish

- Add overlap-aware word dedupe.
- Add logging:
  - segment index
  - cut reason
  - duration
  - queue depth
  - transcription completion time
- Update docs and installer config comments.

## Open Decisions

- Default target: `10s` is reasonable with `large-v3-turbo`; shorter may feel snappier but increases boundary artifacts.
- Default grace: `2s` gives a natural pause a chance without waiting too long.
- Whether to emit text immediately as each worker result completes while key is still held, or buffer output until release. The current product direction suggests immediate output during continuous mode.
- Whether restore-clipboard delay should block future transcript emission. It currently does; longer term, output may also need its own worker.

