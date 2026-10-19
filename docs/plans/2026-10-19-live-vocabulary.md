# Live vocabulary directory

The user requested a directory of vocabulary files that other applications can
manage independently. The running daemon combines their line-based lists and
uses edits, additions, and removals without a restart. Both supported engines
must consume the vocabulary while inference remains offline.

Base: `1f93cd19d6033686b35b540c200aa9930370721e` on clean `main`.
Follow `AGENTS.md`, `.agents/refactor-policy.md`, and `docs/acceptance-tests.md`.

COMPACTION CONTINUITY: Re-read implement-code-change and the task-defining artifacts before continuing after compaction or session restoration.

Review units and gates:

1. Add Phonon-2 hotwords through the pinned fermion-research 0.2.11 encoder ABI 1
   and decoder ABI 3. Keep the model revision unchanged. Verify crate tests,
   real-model bias/clear behavior, Clippy and formatting, then commit.
2. Add CLI/TOML vocabulary directory configuration, deterministic file merging,
   reload before each background transcription, and both engine adapters.
   Verify through the existing daemon acceptance interface, configuration tests,
   and real models. Update user documentation and acceptance mappings, then commit.
3. Resolve per-commit reviews, curate commits, review the full range, run the CI
   quality gates, and push. Retain normal correction commits after final review.

Acceptance checklist:

- [ ] Default directory under the Soundvibes config directory; CLI overrides TOML.
- [ ] Each app owns a UTF-8 `.txt` file, one word or phrase per line.
- [ ] Deterministic merging, case-insensitive deduplication, blank/comment handling.
- [ ] Additions, edits, atomic replacements, and deletions affect the next segment.
- [ ] Missing directory is empty and can be created while the daemon runs.
- [ ] Invalid/unreadable snapshots warn and retain the last successfully loaded list.
- [ ] Whisper receives vocabulary as a prompt; Phonon-2 receives native hotwords.
- [ ] Engine capacity limits are documented and reported rather than silently hidden.
- [ ] Acceptance tests in `tests/acceptance.rs` map to `docs/acceptance-tests.md`.
- [ ] Format, lint, tests, release build, reviews, commits and remote push complete.

Verification record:

- Mise is unavailable. Use the equivalent Cargo commands from `mise.toml`.
- Baseline `cargo test --test acceptance --features test-support at16_phonon2_rejects_unsupported_audio_configuration` passed.
- The hotword API test first failed to compile because `Model::set_hotwords` was absent.
- Upstream 0.2.11 was checked independently on JFK audio. Strength 20 with the
  term `Quillon` changes the output; strength 10 does not reliably do so.
  The production default remains the upstream value of 2.0.
- `cargo test -p phonon2 --release` passed with the real model and new libraries,
  including actual bias, clearing, zero strength, repeated decode and model reload.
- `cargo clippy -p phonon2 --all-targets -- -D warnings`, formatting and diff checks passed.
- The mapped-library setup regression passed with the cached model archive and
  0.2.11 wheel. An initial attempt used a nonexistent archive path; the corrected
  run used the cached reference archive. Existing installed model assets were not modified.
