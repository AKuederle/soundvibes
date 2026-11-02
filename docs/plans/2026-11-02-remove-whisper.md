# Remove the Whisper backend

User request: remove Whisper completely, in the main checkout. Base `9ad0c1e`.
Keep Parakeet, Phonon-2, live streaming/offline fallback, vocabulary refresh,
Linux hotkeys and existing output modes. Do not install/restart the active service.

Remove the engine, FFI shim/bindings/build script/vendor submodule, model download
and size/language configuration, size-only set-model CLI/socket APIs, and obsolete
consumers/tests. Make Parakeet default. Keep real audio as an independent test
fixture. Update current product/setup/acceptance documentation. Dated prior plans
remain historical records, not current backend instructions.

Review units: complete removal with tests and documentation, then executable
packaging simplification if selected. Verify each unit and commit before the next.
Run format, workspace Clippy/tests/release build and configured real Parakeet
recognition. Close per-commit RoboRev jobs, curate the stack, review the full range,
then pull/rebase, push main and verify a clean, up-to-date checkout.
