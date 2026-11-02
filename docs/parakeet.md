# Parakeet Unified English

SoundVibes supports NVIDIA Parakeet Unified EN 0.6B through transcribe.cpp
0.4.1, pinned to Anomaly revision
`7045d1c0fe7cee37747b2792977421d435e75fca`.

Build the single `sv` executable with `cargo build --release`, or install it with
`cargo install --path .`. Parakeet runs inside SoundVibes on the transcription
worker. There is no companion executable or IPC. No service restart or installation
is part of preparing the backend.

Prepare the Q8_0 asset with `python3 scripts/prepare-parakeet.py`. This verifies
731357568 bytes and SHA256
`4b50b6dd862bf6e346929aaf4f5eaacec003bfa3f56462d6c874b41ef2f38795`
from Hugging Face `handy-computer/parakeet-unified-en-0.6b-gguf` revision
`7e948f21b7bdbac698d3318db9d350f1096f3b6c`. Existing assets can be verified with
`--verify-only --path PATH`, then reused directly through `--model PATH`.
The model is subject to the NVIDIA Open Model License. Downloads are explicit;
the daemon never silently downloads this model.

```toml
engine = "parakeet"
model = "/home/arne/.local/share/hex/models/parakeet-unified-en-0.6b-Q8_0.gguf"
parakeet_backend = "auto"
language = "en"
sample_rate = 16000
```

Equivalent CLI flags are `--engine parakeet --model PATH --parakeet-backend auto`.
`auto` selects available acceleration with CPU fallback. `vulkan` requires Vulkan;
`cpu` uses four CPU threads. GPU sessions use one decoder thread. English and
16 kHz mono audio are required. Phonon-2 remains available.
Vocabulary `.txt` files are passed to native Parakeet vocabulary biasing,
including terms such as TanStack and shadcn.

The transcribe.cpp Vulkan build additionally requires SPIRV-Headers development
files, including their CMake package. Install `spirv-headers` with your distro's
package manager. A user-local install can be selected through `CMAKE_PREFIX_PATH`.
For reproducible source preparation, SPIRV-Headers tag `vulkan-sdk-1.4.335.0`
is commit `b824a462d4256d720bebb40e78b9eb8f78bbb305`.
For user-local headers, also set `CPLUS_INCLUDE_PATH=PREFIX/include` because the
pinned ggml build discovers the CMake package but expects headers on the compiler
include path. CPU-only builds use
`cargo build --workspace --no-default-features --release`.

Live inference is enabled by default. Set `parakeet_streaming = false` or
`--parakeet-streaming false` for offline-only operation, useful on slower CPUs.
Live sessions keep decoder state and overlapping encoder windows while capturing
one segment. With `vad = "continuous"`, a pause or duration limit finalizes that
segment and outputs its text while the key is still held. The next stream starts
immediately. Timed cuts carry `segment_overlap_ms` of audio and deduplicate repeated
boundary words, including the last segment emitted on release. Default segment
target/grace are 10/2 seconds; pause detection uses `vad_silence_ms` and
`segment_min_ms`. Other VAD modes finalize on release.

Native encoder contexts default to 5.6 seconds left, 1.04 seconds chunk and 1.04
seconds right. This requires lookahead and is not immediate word-by-word output.
Only finalized segment text enters the existing output path.

SoundVibes retains the current segment for offline retry, about 3.84 MB per minute
at 16 kHz float32. Begin/feed/finalize failures produce a warning and retry the
segment's complete streamed audio offline. The native stream is dropped before
retrying, releasing the session and model compute lease. Disabling streaming keeps
the same continuous pause/duration boundaries with offline inference.
Vocabulary refreshes before each segment begins and again before an offline retry.
The no-speech timeout applies to the initial wait for speech in a hold; later
pauses after recognized speech do not cancel capture. No-speech cancellation
discards the active stream. Shutdown finalizes remaining
audio and drains ordered results.
