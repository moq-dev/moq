# [L] Transport close

## Goal

The Rust connection and QUIC transport state each live in one phase value:

- Once moq-tokio's reconnecting connection gives up, its status stream yields
  the terminal error next, never a `Disconnected` first, even on a handle
  that never connected. The bindings that expose status report the same
  sequence.
- The moq-tokio, moq-uring, and qmux backends agree on when `closed()`
  resolves for a local close.
- A closed or driverless endpoint can't be revived.

## Plan

The shared decisions are in the [epic](/quest/m1/explicit-state/README.md).
Decided 2026-11-07 (maintainer): ✅ `quic::driver::Endpoint::rebind` becomes
crate-private, since only tests call it. Not chosen: refusing to rebind after
close. This is breaking, so if the rest of this child stays additive, split
the `rebind` change into its own m2 quest.

Bugs to reproduce and fix:

- **Trailing `Disconnected`** (`rs/moq-tokio/src/connection.rs`):
  - `Shared::end` calls `state.offline()`, which writes
    `status = Some(Disconnected)` in the same revision that sets `ended = true`.
  - `poll_status` matches only `status`, and kio's `Consumer::poll` runs the
    closure before it reports a closed channel (`rs/kio/src/consumer.rs`). So
    the caller gets `Ok(Disconnected)` first and only then `Err(terminal)`.
  - That contradicts `poll_status`'s doc and `end`'s own comment.
  - Recommended: `enum Phase { Connecting { failure }, Live { session,
    version, transport, migrating: Option<Predecessor> }, Offline { failure },
    Closed(Option<Error>) }`, with `Status` and `Observation` derived from it.
- **`rebind` after close** (`rs/moq-tokio/src/quic/driver/endpoint.rs`):
  - `rebind` checks neither `driver_lost` nor the pending close, and
    `slot.replace(socket)` refills a released slot.
  - The result is a bound port that nothing reads.
  - Recommended: `enum Lifecycle { Running, Closing { code, reason },
    Released, DriverLost }` on the endpoint state, checked by every entry
    point.

Instances to sweep:

- `rs/moq-quic/src/connection/mux/mod.rs`, QMux `Connection`:
  - `params_sent`, `peer`, `error`, `close`, `close_sent`, and a reused
    `conn_state`, with `error.is_some()` checked at about 11 sites.
  - `conn_state` starts as `Established` while handshaking.
  - Recommended: `Phase { Handshake, Established(Params), Closing { error,
    frame }, Closed(error) }`.
- `rs/moq-uring/src/quic/driver/connection.rs` and `rs/qmux/src/session.rs`:
  - `closed`, `local_close`, `dead`, and `close_staged` beside the connection.
  - moq-tokio resolves `poll_closed` on a local close at once, while the
    other two wait for a staged latch. Settle on one behavior.
- `rs/moq-quic/src/connection/streams/recv.rs` and `lease.rs`: `stopped` plus
  `evicted`. Also `send.rs`: the peer's STOP lives only in `stop_reason`, and
  `fin_pending` and `released` sit beside `SendState`.
- `rs/moq-quic/src/connection/paths.rs`: `challenge` plus
  `challenge_pending` plus `validated`.
- `rs/moq-quic/src/connection/mod.rs`: `close` and `draining_reported` beside
  `State`.
- `rs/moq-tokio/src/quic/web/session.rs`: WebTransport and raw sessions share
  one struct of `Option`s. `rs/moq-uring/src/quic/web.rs` already groups
  most of it.
- `rs/moq-relay/src/cluster.rs` (`DialersPhase`) and
  `rs/moq-relay/src/shutdown.rs` (`Serving`, whose `closed()` underflows if
  it is called twice).
- `rs/qmux/src/driver.rs` (`Timer.closing` duplicates `Phase::Closing`) and
  `rs/qmux/src/session.rs` (`SendStream` keeps `fin` and `reset`, and folds
  STOP into `fin`).

Keep the BBR congestion fields that mirror the draft spec as they are unless
a phase enum makes them clearer: they port a published algorithm.

[moq-dev/moq#8139](https://github.com/moq-dev/moq/pull/8139) edits
`connection.rs`'s tests only.
