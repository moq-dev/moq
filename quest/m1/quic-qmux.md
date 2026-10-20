# [XL] Run qmux on the QUIC stream state machine

## Goal

qmux lives in the monorepo and uses `moq-quic`'s stream state machine for
stream lifecycle, flow control, datagram buffering, priority scheduling, and
transport parameters. It owns only record framing, its transport adapters,
and the semantics that differ from QUIC, instead of maintaining a parallel
stream implementation. moq-tokio and moq-relay depend on the in-tree crate.

## Plan

Decided in the 2026-09-30 plan:

- **Scope is the Rust core plus adapters.** Port the prototype core and
  moq-dev/web-transport's TCP, TLS, WebSocket, and UDS adapters, ALPN
  negotiation, byte-stream framing, and socket stats (about 2.3k lines) onto
  it. Its session implements moq-net's `transport::poll` traits instead of
  `web-transport-trait` ([Drop web-transport-trait](/quest/m2/drop-web-transport-trait.md)). The TypeScript `@moq/qmux`
  keeps its own state machine and is the interop reference; it moves into
  `js/` in [TS qmux](/quest/m1/ts-qmux.md).
- **All four wire versions stay**: draft-02, draft-01, draft-00, and the
  legacy `webtransport` format, since published clients (and the TS peer)
  negotiate them. They differ only in record framing, so they share the one
  state machine.
- **Shape**: the sans-IO part is `moq_quic::mux`, a module behind a feature,
  so it reaches the stream state without widening `moq-quic`'s public API.
  The tokio session, adapters, and trait impl are the `qmux` crate, published
  from the monorepo under the existing crates.io name as its next breaking
  version. The web-transport repo's copy stays published, and its
  dependents can move to the new release on their own schedule.

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

`RESET_STREAM_AT` is a QUIC extension, not a qmux-specific frame, and
`moq-quic` now implements it (#5690). Make qmux drive that shared send and receive state
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
bidirectional interoperability against the last web-transport `qmux` release (0.6.x, a
test-only dependency), and the in-tree TypeScript qmux peer used by `js/net`.

Acceptance carried over from the deleted m0 qmux credit quests (web-transport#412 and #413
are no longer pursued): a session returns connection-level credit for every byte it
receives, whether the app reads it, drops the stream unread, or it arrives after
STOP_SENDING, so a long-lived session never stalls on MAX_DATA; and `close()` delivers its
APPLICATION_CLOSE frame before the transport drops, whenever the transport stays writable
within the close bound. Test both. If
[Rust request credit](/quest/m1/rs-request-credit.md) landed first, the session
also implements its non-waiting open, so a request without stream credit fails
at once on qmux too. Preserve rejection of
prohibited QUIC frames, params-first setup, record-size validation, close and
reset semantics (the first recorded close wins, as close codes #4262
settled), keep-alive behavior, and bounded flow-control tests.

There must be one stream state machine in the dependency graph.

Verify minimal, default, and all-feature builds so enabling iroh, qmux, or
the uring runtime never unifies two copies of the stream state.

Decided in the 2026-09-30 audit: moved to m2, since no m1 quest consumes it;
the fork plan the same day kept it there.

Decided 2026-10-20 (maintainer): moved to m1. moq stops depending on the
web-transport repo, so the qmux credit and close fixes ship here instead of
as upstream releases and pin bumps; the bug stays on main until this lands.
The release line keeps its current qmux and gets no backport.

## Required

- [TS qmux](/quest/m1/ts-qmux.md) - the in-tree TypeScript peer this quest's interop acceptance runs against

## Related

- [tls:// peer certificates](/quest/m2/tls-listener-mtls.md) - adds a peer-certificate accessor to the in-tree qmux TLS session
