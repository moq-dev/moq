# [M] Require RESET_STREAM_AT for lite-07 group streams

## Goal

`moq-net::transport::poll::SendStream`, `moq-tokio`, `rs/qmux`, and `js/qmux` expose `reset_at(offset, code)`. On lite-07, every publisher-initiated reset of an opened Group Stream calls `reset_at(header_end, code)` and emits no `SUBSCRIBE_DROP` on the Subscribe Stream, eliminating header-loss races, post-FIN `SUBSCRIBE_DROP(OLD)` contradictions, and erroneous `STOP_SENDING` drop echoes.

## Plan

Decided 2026-11-24 (maintainer, /quest-plan): ✅ Require `RESET_STREAM_AT` on lite-07 (`reset_at(header_end, code)`) for opened Group Streams with no `SUBSCRIBE_DROP` on publisher resets, accepting the rare browser WebTransport race and relying on `moq-quic`'s 1-RTT header-ACK hold when `!peer_reset_stream_at` (rejected: keeping `SUBSCRIBE_DROP` fallback whenever `!peer_reset_stream_at`; keeping `SUBSCRIBE_DROP` on every reset).

Root causes fixed (audit findings W2, W3, W4):

- **Transport plumbing (`W4`):** `moq-quic` and `moq-quic::connection::mux` (`QMux02`) already implement `reset_at` (including the 1-RTT header-ACK wait fallback when the peer did not negotiate QUIC transport parameter `0x1D`), and `moq-tokio::quic::web::send` already uses it for HTTP/3 WebTransport stream headers. Expose `reset_at(offset: u64, code: u32)` on:
  - `rs/moq-net/src/transport.rs` (`transport::poll::SendStream`)
  - `rs/moq-tokio/src/quic/driver/send.rs` and `rs/moq-tokio/src/quic/web/send.rs` (accounting for the WebTransport stream-header byte prefix when computing the underlying QUIC offset)
  - `rs/qmux/src/session.rs` (`qmux-02` send support)
  - `js/qmux` (currently decode-only; add `RESET_STREAM_AT` encode support for `qmux-02`)
  - `rs/moq-uring` and any mock/sim `SendStream` implementations used in `moq-net` tests
- **Publisher reset contract (`W2`):**
  - Record `header_end` immediately after encoding the `GROUP` header on a lite-07 Group Stream.
  - Once `header_end` is written, every publisher-initiated abort (`OLD`, `TOO_FAR_BEHIND`, `EVICTED`, or post-FIN `OLD` while `GroupState::Closed` awaits FIN ACK) calls `reset_at(header_end, code)` and **never** pushes a `SUBSCRIBE_DROP` to the Subscribe Stream.
  - If `FIN` arrived at the peer first, QUIC ignores `RESET_STREAM_AT`, so the peer receives the complete group with zero contradictory `SUBSCRIBE_DROP(OLD)`. If `RESET_STREAM_AT` wins, QUIC delivers `0..header_end` before surfacing `Reset(code)`.
  - `SUBSCRIBE_DROP` is emitted only for groups that never opened a Group Stream, or by browser WebTransport publishers whose W3C `WritableStream.abort()` lacks `resetAt`.
- **Subscriber `STOP_SENDING` handling (`W3`):**
  - Distinguish subscriber-initiated `STOP_SENDING` from publisher-initiated resets in `GroupServe` (`rs/moq-net/src/lite/publisher.rs` and `js/net/src/lite/publisher.ts`).
  - Never echo `STOP_SENDING(Duplicate)` or `STOP_SENDING(Cancel)` back as a permanent `SUBSCRIBE_DROP` on the control stream.
  - Only when a peer sends `STOP_SENDING(Evicted)` before acknowledging the `GROUP` header may the publisher emit `SUBSCRIBE_DROP` for that group (so overcommitted receive windows still account for pre-header evictions without stalling).
- **Cross-package sync and verification:**
  - Update Rust (`rs/moq-net`, `rs/moq-tokio`, `rs/qmux`, `rs/moq-uring`), JS (`js/net`, `js/qmux`), and `drafts/draft-lcurley-moq-lite.md` in the same PR.
  - Add deterministic unit and sim tests covering: reset immediately after header write, post-FIN `OLD` expiry race (asserting no contradictory `SUBSCRIBE_DROP(OLD)`), `STOP_SENDING(Duplicate)` (asserting no drop echo), `STOP_SENDING(Evicted)` pre-header, and `qmux-02` `RESET_STREAM_AT` round-trip.
  - Run `just test interop --all`.

Public API: `SendStream::reset_at` added to internal/transport stream traits (`moq-net`, `qmux`, `@moq/qmux`). Wire: lite-07 requires `RESET_STREAM_AT` semantics for opened Group Streams (no `SUBSCRIBE_DROP` on publisher reset); pre-lite-07 unchanged.

## Related

- [Browser reset reports](/quest/m4/browser-reset-reports.md) - upstream Chromium and Firefox WebTransport reset bugs whose rare race lite-07 accepts
- [The overcommitted receive-window cells run again](/quest/m3/receive-windows-evicted.md) - relies on pre-header `STOP_SENDING(Evicted)` accounting
