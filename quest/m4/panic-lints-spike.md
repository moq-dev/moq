# [M] Spike panic-preventing Clippy lints on untrusted-input crates

## Goal

Run a spike enabling Clippy's panic-preventing restriction lints (`unwrap_used`, `indexing_slicing`, `arithmetic_side_effects`, and `panic`) on `moq`'s untrusted-input boundary crates (`moq-net`, `moq-quic`, `moq-hls`, `moq-mux`, `moq-auth`, and `moq-relay`) to triage every finding, fix clear small bugs (and file `m0`/`m1` quests for larger ones), and report whether any subset of these lints earns a permanent CI gate.

Non-goals: changing public APIs or wire formats, banning documented `.expect(...)` or `unreachable!(...)` invariants, enabling the lints permanently in this PR, or adopting Kani, Flux, or `no-panic` here.

## Plan

Decided 2026-11-27 (maintainer, `/quest-plan`):

- ✅ Crate scope: untrusted-input boundary crates (`moq-net`, `moq-quic`, `moq-hls`, `moq-mux`, `moq-auth`, `moq-relay`) (rejected: whole `rs/` workspace; single canary crate first). These six crates parse peer and media input directly and account for the bulk of recent panic and overflow bugs.
- ✅ Lint set: `clippy::unwrap_used`, `clippy::indexing_slicing`, `clippy::arithmetic_side_effects`, and `clippy::panic`, keeping documented `.expect(...)` and `unreachable!(...)` (rejected: dropping `arithmetic_side_effects` in favor of `integer_division` only; forbidding `expect_used` and `unreachable` too). `arithmetic_side_effects` also covers integer division and remainder while respecting `NonZero` and constants.
- ✅ Deliverable: report signal-to-noise and fix clear, small reachable panic bugs in this PR (filing `m0`/`m1` follow-ups for larger fixes), leaving the lints off in `main` until a follow-up decides which ones graduate to CI (rejected: enabling lints directly in the spike PR; report-only with no bug fixes).

Milestone m4: `m9` was retired on 2026-11-26 and internal tooling spikes now live in m4 behind the release cut, beside [Kani codec proofs](/quest/m4/codec-kani.md).

Guidance for the implementer:

- `moq`'s test build doubles as the clippy gate, so run the spike with test-allowing `clippy.toml` keys (`allow-unwrap-in-tests = true`, `allow-indexing-slicing-in-tests = true`, `allow-panic-in-tests = true`) or `--lib --bins` so test code does not drown out production findings.
- Classify findings per crate and per lint: confirmed reachable panic bug, latent bug / fragile invariant, type-improvable site (`NonZero`, `as_chunks`, iterators, `NonEmpty`), or pure noise.
- Pay special attention to peer-controlled lengths, timestamps, counters, and packet/segment slicing, which produced the panics tracked in [stats-aggregate-fixes](/quest/m1/stats-aggregate-fixes.md) and [quic-lease-ownership](/quest/m2/quic-lease-ownership.md).
- Keep the PR summary concise with the per-lint counts, every real bug found (fixed here or filed as an `m0`/`m1` follow-up), and a concrete recommendation on which lints (if any) have high enough signal-to-noise to gate in CI as a follow-up quest.

Public API: none. Wire: none.

## Related

- [Kani proves the varint codecs and length arithmetic for every input](/quest/m4/codec-kani.md) - formal panic-freedom proofs for `moq-net` varints and peer length arithmetic
- [Audit code hygiene](/quest/m3/audit-code.md) - recurring lint and suppression audit
