# [M] Prove QMux connection liveness, driver flush ordering, and Rust-to-JS wire conformance

## Goal

TLA+ models under `rs/moq-net/verify/` prove the unified connection liveness/keep-alive/close-drain machine, driver wakeup/flush ordering (`Phase`, `RecvWake`, and flush-confirmed `transmitted()`), and ordered `RESET_STREAM_AT` + `STOP_SENDING` races, and a deterministic Rust-to-JS conformance vector suite replays identical wire and state-machine edge cases against both `moq_quic::connection::mux::Connection` and `js/qmux` in CI.

Boundaries and non-goals: no WASM compilation of `js/qmux`; conformance stays deterministic and fast enough for the PR merge gate without sleeping or live sockets.

## Plan

Decided 2026-11-26 (maintainer, `/quest-plan` of the QMux audit): ✅ Confirm full scope including TLA+ proofs and Rust-to-JS conformance vectors (rejected: Correctness and web-transport ports only; Drop).

Guidance:

- Follow the layout and TLC harness conventions in `rs/moq-net/verify/` (matching `verify-credit`, `verify-reconnect`, `verify-resume`, `verify-subscribe`, `verify-failover`, `verify-warm`) and wire model checking into `just` and CI for paths it guards.
- Model and check:
  1. Connection liveness, soft/hard keep-alive, one-way-sender probing, and close-drain termination (no premature idle kill of active senders, no missed dead-peer timeout, no deadlock during `Phase::Flushing` / `Phase::Closing`).
  2. Driver wakeup and flush ordering (`RecvWake`, `poll_flush`, flush-gated `transmitted()` / `reset_acked()`).
  3. Ordered `RESET_STREAM_AT` + `STOP_SENDING` + buffered reliable prefix races.
- Add a shared checked-in conformance vector suite (wire bytes, scripted events, expected outgoing frames/errors/states) replayed deterministically by both Rust unit tests against `moq_quic::connection::mux::Connection` and `bun test` in `js/qmux`, covering `qmux-00`/`01`/`02` framing, transport parameters, `QX_PING` coalescing and monotonicity, `RESET_STREAM_AT`, `FINAL_SIZE_ERROR`, and close handling.

Public API: none. Wire: none.

## Required

- [Unify QMux and moq-quic sans-IO connection state machines](/quest/m4/qmux-consolidation/core.md) - models and tests the unified state machines
