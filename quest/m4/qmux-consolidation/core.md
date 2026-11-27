# [M] Unify QMux and moq-quic sans-IO connection state machines

## Goal

`moq-quic` owns one shared implementation of each connection-level state machine used by both QUIC and QMux:

- One liveness, idle-timeout, and keep-alive timer machine (absorbing `keep-alive`'s soft/hard thresholds across QUIC and QMux).
- One transport-parameter codec with an explicit QUIC vs QMux profile, removing `rs/moq-quic/src/connection/mux/params.rs`'s divergent second pass.
- One shared `Datagrams` wrapper and one unified stream-ack, ordered `RESET_STREAM_AT`, and close-drain contract.

Boundaries and non-goals: internal refactor inside `rs/moq-quic` and `rs/qmux`; no wire changes and no breaking public API changes.

## Plan

Decided 2026-11-26 (maintainer, `/quest-plan` of the QMux audit): ✅ Unify sans-IO core AND shared async Session/Stream handles now (rejected: defer async handles to `quic-sansio-driver`).

Guidance:

- Fold `mux::Timer` and QUIC's idle/keep-alive timer into one timer module parameterized only by transport capability (ACK-eliciting packets vs `QX_PING` records, plus optional backpressure deferral), so soft/hard keep-alive and one-way-sender liveness cannot diverge again.
- Parameterize `TransportParameters` decode/encode by profile (`Quic` vs `QMux(Version)`) so duplicate-parameter rejection, forbidden QUIC v1 parameters, `max_record_size`, and `application_protocols` run in one pass with no QUIC-only checks (`min_ack_delay`, receive timestamps) applied to QMux blobs.
- Align `Datagrams`, stream flush-ack confirmation, ordered `RESET_STREAM_AT` serialization, and close-drain phase handling so `Connection` and `mux::Connection` expose one consistent contract to the shared async handle layer in [handles](/quest/m4/qmux-consolidation/handles.md).

Public API: none. Wire: none.

## Required

- [Fix Rust and JS QMux correctness bugs and port web-transport fixes](/quest/m1/qmux-correctness-fixes.md) - lands the one-way-sender idle-timeout, ordered `RESET_STREAM_AT`, and flush-ack semantics before unifying them
- [Session keep-alive with a stale signal](/quest/m1/keep-alive.md) - lands the soft/hard keep-alive semantics the unified timer absorbs
