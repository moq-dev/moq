# [M] Offload server TLS handshakes off the moq-uring packet loop

## Goal

`moq-uring` splits server handshake acceptance (`start_accept` -> background/offloaded TLS `ServerHello` + cert signing -> `finish_accept`, matching `moq-tokio`) so incoming connection bursts no longer stall RX/TX latency for live connections on the same `io_uring` worker shard.

## Plan

Decided 2026-11-26 (maintainer, `/quest-plan` of the `moq-quic` audit): ✅ include async handshake offload in `moq-uring` among the planned net-new features (rejected: leaving it out of this batch). Joins `m4` as a runtime latency improvement after the 0.22 cut.

Problem found in the audit (`rs/moq-uring/src/quic/driver/endpoint.rs:395`):

- `moq-uring`'s `greet()` calls synchronous `self.endpoint.borrow_mut().accept(incoming, now, buf)` inline inside `demux()`.
- Full TLS key exchange (X25519 / post-quantum ML-KEM) and certificate signing run synchronously on the single-threaded `io_uring` worker loop, blocking every active connection on that shard during reconnect storms or connection floods.
- `moq-quic` already exposes the split `start_accept` / `Accepting::accept` / `finish_accept` API that `moq-tokio` uses.

Guidance:

- Move `Accepting::accept()` onto a small dedicated handshake worker pool (bounded by `backlog`, returning completions to the owning shard via its existing wake mechanism) while keeping `start_accept` and `finish_accept` on the shard thread.
- Measure tail latency of live media streams on a `moq-uring` shard while bombarding it with new TLS handshakes before and after.

Public API: none. Wire: none.

## Related

- [QUIC I/O boundary](/quest/m5/quic-io-boundary.md) - `moq-uring` receive/transmit seam
