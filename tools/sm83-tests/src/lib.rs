//! Instruction-level tests for the SM83 CPU in `gbc-core`, using the
//! SingleStepTests/sm83 suite (1000 randomized cases per opcode, with the
//! bus activity of every M-cycle). The tests live in `tests/`; this crate
//! has no library code.
//!
//! The data is downloaded by `scripts/fetch-sm83-tests.sh` into
//! `test-data/sm83/` (gitignored). Without it the tests skip with a
//! message, so `cargo test` works offline, unless `GBC_REQUIRE_TEST_DATA=1`
//! is set (then they fail). Every file is checked against the committed
//! per-file hashes in `sm83-v1.sha256` before it is used.
