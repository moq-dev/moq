# Consolidate QMux and moq-quic state machines and verify them

## Goal

Eliminate the remaining duplicated connection and async handle state machines between `rs/qmux`, `rs/moq-quic`, `rs/moq-tokio`, and `rs/moq-uring`, and prove the unified state machines with TLA+ models and deterministic Rust-to-JS wire conformance vectors.

Boundaries and non-goals: no wire breaks, no breaking public API changes, and no WASM compilation of `js/qmux`.

## Plan

Decided 2026-11-26 (maintainer, `/quest-plan` of the QMux audit):

- Goal: ✅ Confirm full scope including consolidation and formal verification (rejected: Correctness and web-transport ports only; Drop).
- Async handle scope: ✅ Unify sans-IO core AND shared async Session/Stream handles now (rejected: Unify sans-IO core + TLA+/conformance now; defer async handles to `quic-sansio-driver`). `quic-sansio-driver` is deferred, and unifying the handle layer now lets one implementation and one TLA+ model cover both QUIC and QMux.

Joins `m4` (behind the release cut) because consolidation and formal proofs are internal architecture and verification work with no blocking release consumer. Split into three independently completable quests: sans-IO core unification (`core.md`), shared async handles (`handles.md`), and TLA+ plus Rust-to-JS conformance verification (`verify.md`).

## Related

- [A runtime-free QUIC driver](/quest/m4/quic-sansio-driver.md) - builds on the shared handle layer later
- [Fuzz and benchmark coverage for moq-quic](/quest/m4/quic-fuzz-and-bench-coverage.md) - complementary `moq-quic` mux fuzz target
