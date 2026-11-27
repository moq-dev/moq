# [M] Share one async Session and Stream handle layer across qmux, moq-tokio, and moq-uring

## Goal

`rs/qmux`, `rs/moq-tokio`, and `rs/moq-uring` share one async handle and lifecycle abstraction (`Session`, `SendStream`, `RecvStream`, `RecvLease`, `SendGroup`, and close latch) over `moq_quic::connection::Connection` and `moq_quic::connection::mux::Connection`, eliminating ~800 LOC of duplicated state transitions while keeping each crate's public API and behavior intact.

Boundaries and non-goals: covers the handle/session lifecycle layer, not the UDP/TCP/io_uring I/O loops or endpoint tables (which stay in each transport crate / [quic-sansio-driver](/quest/m4/quic-sansio-driver.md)); no breaking public API or wire changes.

## Plan

Decided 2026-11-26 (maintainer, `/quest-plan` of the QMux audit): ✅ Unify sans-IO core AND shared async Session/Stream handles now (rejected: Unify sans-IO core + TLA+/conformance now; defer async handles to `quic-sansio-driver`).

Guidance:

- Extract the common locked-state handle layer (stream send/recv/lease polling, `SendGroup`, terminal error publication, and `poll_closed` latch) behind a narrow internal trait implemented by both `moq_quic::connection::Connection` and `moq_quic::connection::mux::Connection`.
- Replace the duplicated handle implementations in `rs/qmux/src/session.rs`, `rs/moq-tokio/src/quic/driver/{connection,send,recv}.rs`, and `rs/moq-uring` with wrappers over that shared layer, keeping transport-specific extras (TLS handshake metadata, `SocketStats`, UDP path metrics) thin.
- Verify no regression in lock contention or wakeups across `qmux`, `moq-tokio`, and `moq-uring` test suites.

Public API: none. Wire: none.

## Required

- [Unify QMux and moq-quic sans-IO connection state machines](/quest/m4/qmux-consolidation/core.md) - establishes the unified connection-level contract the shared handles wrap

## Related

- [A runtime-free QUIC driver](/quest/m4/quic-sansio-driver.md) - will build on the shared handle layer
