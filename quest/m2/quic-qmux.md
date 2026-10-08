# [XL] Run qmux on the QUIC stream state machine

## Goal

qmux lives in the monorepo and uses `moq-quic`'s stream state machine for
stream lifecycle, flow control, datagram buffering, priority scheduling, and
transport parameters. It owns only record framing, its transport adapters,
and the semantics that differ from QUIC, instead of maintaining a parallel
stream implementation. moq-tokio and moq-relay depend on the in-tree crate,
and moq-dev/web-transport no longer carries a Rust qmux.

## Plan

Decided in the 2026-09-30 plan:

- **Scope is the Rust core plus adapters.** Port the prototype core and
  moq-dev/web-transport's TCP, TLS, WebSocket, and UDS adapters, ALPN
  negotiation, byte-stream framing, and socket stats (about 2.3k lines) onto
  it, along with the `web-transport-trait` impl. The TypeScript `@moq/qmux`
  and `@moq/web-socket-stream` stay in moq-dev/web-transport with their own
  state machine and become the interop reference.
- **All four wire versions stay**: draft-02, draft-01, draft-00, and the
  legacy `webtransport` format, since published clients (and the TS peer)
  negotiate them. They differ only in record framing, so they share the one
  state machine.
- **Shape**: the sans-IO part is `moq_quic::mux`, a module behind a feature,
  so it reaches the stream state without widening `moq-quic`'s public API.
  The tokio session, adapters, and trait impl are the `qmux` crate, published
  from the monorepo under the existing crates.io name as its next breaking
  version. moq-dev/web-transport removes its copy; the external dependents
  (atmoq-moq-native, web-transport-ws) can move to the new release on their own schedule.

Start from the green
[kixelated/quinn#2](https://github.com/kixelated/quinn/pull/2) prototype,
which is already quinn-based. Rebase it onto `moq-quic` and preserve its
central invariant: drive the
existing stream state machine through the same receive and write entry points
as QUIC, then treat serialization to the reliable underlying transport as the
acknowledgment. Do not copy the stream maps, flow-control accounting, reset
state, or datagram queues into a qmux-specific implementation.

Keep the shared-core patch narrow. The prototype needs only module wiring, a
receive-offset accessor, frame-iterator access, and limited datagram helper
visibility. Review each exposure as a reusable internal boundary rather than
making the whole QUIC connection public.

`RESET_STREAM_AT` is a QUIC extension, not a qmux-specific frame. Once the
reliable-reset quest lands, make qmux drive that shared send and receive state
instead of retaining the prototype's local parsing and transitions. Because
qmux runs over a reliable ordered transport, serialization acknowledges the
committed prefix immediately, but the receiver must still delay the reset
until that prefix is available. Remove the qmux prototype's local
`RESET_STREAM_AT` state once the shared core owns it (moved here from
reliable reset in the 2026-10-05 audit).

Carry the hierarchical send groups from the scheduler quest into qmux's
record writer. Qmux over TCP, TLS, WebSocket, Unix sockets, and in-memory
duplex transports must produce the same subscription fairness and intra-group
ordering as raw QUIC. This quest owns qmux integration of the scheduler's
reusable acceptance fixtures, including byte fairness, strict priority,
newest/oldest group ordering, cancellation, and blocked-stream behavior.
The native scheduler does not wait for this dependent proof.

Add the missing wire evidence before release: golden vectors for each of the four versions,
bidirectional interoperability against the last web-transport `qmux` release (0.6.x), and
the TypeScript qmux/WebSocket peer used by `js/net`. Preserve rejection of
prohibited QUIC frames, params-first setup, record-size validation, close and
reset semantics (the first recorded close wins, as close codes #4262
settled), keep-alive behavior, and bounded flow-control tests.

There must be one stream state machine in the dependency graph.

Verify minimal, default, and all-feature builds so enabling iroh, qmux, or
the uring runtime never unifies two copies of the stream state.

Decided in the 2026-09-30 audit: moved to m2, since no m1 quest consumes it;
the fork plan the same day kept it there.

## Required

- [Hard fork](/quest/m1/quic/fork/README.md) - qmux drives `moq-quic`
- [Reliable stream reset](/quest/m1/quic/reliable-reset.md) - qmux reuses the
  extension's stream state rather than implementing reset locally
- [Hierarchical stream scheduling](/quest/m1/quic/scheduler.md) - qmux must
  expose the same scheduling contract as native QUIC

## Related

- [noq#812](https://github.com/n0-computer/noq/issues/812) - the qmux proposal to n0
- [tls:// peer certificates](/quest/m2/tls-listener-mtls.md) - needs a peer-certificate accessor on qmux's TLS session, upstream or in-tree
