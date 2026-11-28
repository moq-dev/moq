# [L] Split moq-net unit tests out of rs/moq-net/src

## Goal

Move `moq-net`'s large `#[cfg(test)]` suites out of `rs/moq-net/src/` into `moq-net-tests` (or parallel integration test crates) so `moq-net`'s library compiles 3-5x faster and unit-test edits rebuild zero of its 41 downstream crates, with identical test coverage and no public API or runtime behavior changes.

Non-goals: changing `moq-net`'s public API, wire protocol, or runtime semantics; dropping test coverage.

## Plan

Decided 2026-11-26 (maintainer, `/quest-plan` after 24h multi-host CPU audit): ✅ split `moq-net`'s inline `#[cfg(test)]` suites out of `rs/moq-net/src/` into external test crate(s) in `m1` (rejected: leaving all unit tests inline in `rs/moq-net/src/`). Moved to `m3` on 2026-11-28, since it need not block `0.22.0`.

Baseline (2026-11-26):
- `rs/moq-net/src/` is 301,932 LOC total, of which 240,223 LOC (79.6%) is `#[cfg(test)]` vs 61,709 LOC of library code (`model/track.rs` = 40,038 LOC, `ietf/publisher.rs` = 22,959 LOC, `ietf/subscriber.rs` = 20,443 LOC).
- `moq_net` alone consumed 605 CPU-min (10.1 CPU-h) in 24h across `fedora`, `desktop`, and `nixos`, and its single-threaded frontend (~51s) sits on the critical path of every edit while cascading rebuilds to all 41 downstream crates in `rs/`.
- `rs/moq-net-tests` already exists for `fuzz`/JSON tests (`rs/moq-net-tests/Cargo.toml`), and `moq-net` already carries a `test-support` feature.

Approach:
1. Audit `rs/moq-net/src/` test modules by size (starting with `model/track.rs`, `ietf/publisher.rs`, `ietf/subscriber.rs`, `route/tests/`, and `session/`).
2. Move behavioral tests that only need public or `test-support` items into `rs/moq-net-tests` (splitting into multiple `[[test]]` binaries or sibling test crates only where parallel compile time wins without inflating link cost). Keep truly white-box unit tests that access private internals only when moving them would widen default public API; where narrow helpers are needed, gate them behind `#[cfg(feature = "test-support")]` with doc comments (since `sh/text.sh` forbids `#[doc(hidden)]`).
3. Verify `sh/rs/select.sh` stops selecting `moq-net`'s 41 downstream crates when editing moved tests under `rs/moq-net-tests/`.
4. Measure and report before/after `moq_net` `cargo nextest run --no-run` CPU and wall time on (a) a test-only edit and (b) a 1-line edit to `rs/moq-net/src/lib.rs`, plus verify full test count parity.
