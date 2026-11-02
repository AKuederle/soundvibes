# Parakeet Unified English

SoundVibes supports NVIDIA Parakeet Unified EN 0.6B through transcribe.cpp
0.4.1, pinned to Anomaly revision
`7045d1c0fe7cee37747b2792977421d435e75fca`.

Build both binaries with `cargo build --workspace --release` and keep `sv` and
`sv-parakeet` beside each other when installing. The persistent companion owns
transcribe.cpp in a separate process because Whisper embeds another ggml version.
Model inference and IPC remain local. No service restart or installation is part
of preparing this backend.

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
16 kHz mono audio are required. Whisper and Phonon-2 remain available.
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
Live sessions keep decoder state and overlapping encoder windows during recording,
then finalize on release. Defaults are 5.6 seconds left context, 1.04 seconds chunk
and 1.04 seconds right context. This requires lookahead and is not immediate
word-by-word output. Only the final transcript enters the existing output path.

Live recordings finalize once per hold, bypassing pause/duration segmentation.
SoundVibes retains the full recording for offline retry, about 3.84 MB per minute
at 16 kHz float32. Begin/feed/finalize failures produce a warning and retry the
whole recording offline. Failed companion processes are reaped and restarted for
the retry. Disabling streaming retains existing offline VAD segmentation.
Vocabulary refreshes at stream begin and again before an offline retry; edits
during a recording apply to the next recording. No-speech cancellation discards
the stream. Shutdown finalizes the active recording and drains ordered results.
