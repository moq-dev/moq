# [L] moq-quic owns its waits

## Goal

moq-quic's connection, streams, and datagrams expose every wait as a
`poll_*` taking `&kio::Waiter`, and its own state wakes the waiters as
`handle`, timeouts, and application calls change it. `StreamEvent` and the
waits in the connection's `Event` are gone, and moq-tokio's
`quic::driver`, moq-uring's driver, and qmux delete their per-stream and
per-connection waiter tables, keeping only I/O, timers, and their lock.
moq-quic still does no I/O and reads no clock.

## Plan

Landed in [moq-dev/moq#7391](https://github.com/moq-dev/moq/pull/7391): moq-quic
owns the lists and wakes them, over QUIC and QMux alike, alongside the events.
`connection/waits.rs` holds them, owned by `StreamsState`; `StreamsState::notify`
wakes the list matching each `StreamEvent` it queues, and `StreamsState::unblock`
wakes connection-blocked writers as soon as the window has room rather than when
`poll` runs. The poll forms are `recv::Chunks::poll_next`,
`recv::Stream::poll_lease`, `send::Stream::poll_write`, `poll_end`, and
`poll_acked`, `Streams::poll_accept` and `poll_open`, `Datagrams::poll_recv`, and
`Connection::poll_connected`, `poll_handshake_confirmed`, and `poll_closed`, with
`ConnectionLost` on `send::Error` and `recv::Error` for a stream wait that
outlives its connection. Counting-waiter tests cover each wait, two waiters on
one condition, a dropped waiter, and draining after a loss.

What remains:

- Move moq-tokio's `quic::driver`, moq-uring's driver, and qmux onto the poll
  forms, deleting their waiter tables, `Notify`s, and `stopped` map.
- Delete `StreamEvent` and the waiting `Event` variants, turning `notify`'s
  event-to-list mapping into direct wakes, and move each `Event` test that
  asserted a wait to a counting waiter.
- Make `poll_transmit` the driver's wait, below.
- The close-while-output-blocked qmux test and the benches, below.

Decided with the maintainer on 2026-10-25, from "would moq-quic benefit from
kio?":

- **The core owns the waits.** Today moq-quic reports
  `StreamEvent::{Opened, Readable, Writable, Finished, Acked}` and
  connection `Event`s, and each consumer keeps its own tables
  (`blocked_readers`/`blocked_writers` and `Notify`s in moq-tokio,
  `readable`/`writable`/`finishing`/accept/open/datagram `WaiterList`s in
  moq-uring and qmux), about 25 wake sites each, and looks every stream id
  up a second time. The waiter lists move into moq-quic's per-stream and
  per-connection state instead. s2n-quic's transport is prior art: wakers in
  stream state, I/O in pluggable providers.
- **`&kio::Waiter`, the `rs/AGENTS.md` convention.** kio is already public
  through moq-net (`pub use kio`), so a kio break already breaks the stack.
  The caller owns the waiter, so an abandoned poll leaves nothing behind.
- **Every wait.** Accept (per direction), open (stream credit), read, write,
  finished (FIN acknowledged or STOP_SENDING), acked (the
  [poll_acked](/quest/m2/quic-ack-hook.md) offset wait), datagram receive,
  connected, handshake confirmed, and closed. `Event` keeps only
  notifications no handle waits on, or goes if none remain. Endpoint-level
  plumbing (`EndpointEvent`, `ConnectionEvent`) is not a wait and stays.
- **`poll_transmit` is the driver's wait.** It takes the driver's waiter and
  returns `Poll<Transmit>`: with nothing sendable it registers the waiter,
  which a write, open, reset, stop, close, or credit change wakes. Pacing
  and loss timers stay on `poll_timeout`. This replaces the kick each
  handle sends its driver after mutating the connection.
- **After [moq-dev/moq#6950](https://github.com/moq-dev/moq/pull/6950)**, whose queryable
  send-stream end is what `poll_finished` and `poll_acked` read, and whose
  names the new methods take.

Constraints:

- A wake only schedules. moq-quic wakes under the consumer's lock (the
  tokio driver's `Mutex`, moq-uring's and qmux's `RefCell`), as the drivers
  do today, so a waker that polled inline would re-enter it.
- A driver stalled on output still hears a local close. `poll_transmit`
  registers only when nothing is sendable, but qmux's `poll_stalled`
  parks on its `driver` list while a batch is blocked in `feed`/`flush`, and
  a local close wakes it to start the bounded close timer. Keep that through
  a wait that consumes no transmit (a close wait or a non-consuming
  readiness wait), and test it on paused time: close while output is
  blocked by a non-reading peer, and the session still shuts down within
  the close bound.
- Waits drain before failing: accepted streams, readable data, and received
  datagrams stay available after a close until taken, then the error.
- Tests in moq-quic count wakes with a counting waiter instead of matching
  events: two waiters on one condition both wake, a dropped waiter leaves no
  live registration, and each `Event` test that asserted a wait moves over.
- Measure: `rs/moq-quic/benches/streams.rs` and a driver fanout bench
  before and after, swept over streams per connection, so the per-event cost
  shows as a slope if the lists cost more than the maps they replace.

Public API: moq-quic gains `poll_*` methods taking `&kio::Waiter`,
`poll_transmit` changes shape, and `StreamEvent` and the waiting `Event`
variants go. The drivers' and qmux's public APIs are unchanged here; the
driver poll forms come in [poll](/quest/m1/quic-api/poll.md). Update
moq-quic's README fork list and describe migration in the PR's Impact section. Wire: none.

## Related

- [Poll forms](/quest/m1/quic-api/poll.md) - the drivers' public poll forms forward to these
- [poll_acked](/quest/m2/quic-ack-hook.md) - its offset wait lives here
- [QUIC lock contention](/quest/m2/perf/quic-lock-contention.md) - profiles the lock these waits are registered under
