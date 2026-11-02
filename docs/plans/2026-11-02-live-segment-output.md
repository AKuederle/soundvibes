# Restore continuous output with live Parakeet

User reports no output on pauses or long holds after GPU deployment. Base a020b03,
main checkout only. Preserve native GPU inference during recording, all output and
hotkey settings, vocabulary, ordering, cancellation and offline fallback.

Reproduced through agreed acceptance/worker seams: deterministic streaming-engine
pause and duration cases both emitted only after release. Ranked hypotheses were
the daemon segmentation gate, native lookahead, and output delivery. The engine
and output adapters ruled out the latter two. The live-mode exclusion in the
daemon disabled all continuous cuts. This was introduced with the initial backend.

Fix continuous mode to finalize live streams on the same VAD/time boundaries as
offline engines, queue the next native session and seed carried overlap. Preserve
the overlap flag on release. Full-buffer offline fallback must match audio already
fed into the stream, including pause silence. Keep partial hypotheses out of text
injection; emit finalized segments before release.

One review unit: fix with deterministic acceptance for pause, long hold, exact
carry and release deduplication; asset-gated native pre-release output; docs update.
Verify format/Clippy/workspace tests/release build, close per-commit RoboRev,
review full task range, push, install verified binary and restart the already
configured Parakeet Vulkan service. Preserve F24, ydotool and personal config.
