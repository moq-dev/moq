# [M] The QUIC driver's waits poll without leaking

## Goal

Every wait in `moq_tokio::quic::driver` has a `poll_*` form that leaks
nothing: `Connection::poll_open_uni`, `poll_open_bi`, `poll_accept_uni`,
`poll_accept_bi`, `poll_recv_datagram` and `poll_closed`,
`SendStream::poll_closed` (today's `stopped`), and `Endpoint::poll_accept`.
`SendStream::poll_acked`, which [poll_acked](/quest/m2/quic-ack-hook.md)
adds, uses the same mechanism. The names follow moq-uring's driver, which
already follows moq-net's `transport::poll` traits; return types stay the
driver's own. A waiter that polls once and is dropped leaves nothing alive:
no live registration and no task.

## Plan

The driver's waits are futures over `tokio::sync::Notify`: the connection's
`Shared` fields, the per-stream `stopped` map, and the endpoint's `incoming`.
`Notify` cannot hold a bare waker, so a poll-based caller has to keep the
future, and moq-tokio's `transport::Session` adapter boxes one per pending
operation. [Drop web-transport-trait](/quest/m2/drop-web-transport-trait.md)
wants a native `poll::Session` on the QUIC backend, and
[poll_acked](/quest/m2/quic-ack-hook.md) wants a real poll method. Both need
these.

The trap is a stateless `&self` poll that pushes the caller's waker into a
shared list. Nothing removes the entry when its caller gives up, so it lives,
holding its task, until the next signal; for `poll_closed` that is the end of
the connection. A trial of that shape kept all 1000 abandoned `poll_closed`
waiters. Leak-free needs per-caller state that dies with the caller, as
tokio's `Notified` has, and as kio's `Park` and `WaiterList` give through
`Weak` entries.

Options:

- **A (recommended): `&mut self, cx`, with a `kio::Park` in each handle.** A
  clone gets an empty park, so a registration belongs to the handle that made
  it and dies with it. This is moq-uring's driver shape and moq-net's
  `poll::Session` shape, so a native impl forwards in one line, and kio stays
  private.

  ```rust
  impl Connection {
      pub fn poll_accept_uni(&mut self, cx: &mut Context<'_>) -> Poll<Result<RecvStream, ConnectionError>>;
      pub fn poll_open_uni(&mut self, cx: &mut Context<'_>, options: SendOptions<'_>) -> Poll<Result<SendStream, ConnectionError>>;
      pub fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<ConnectionError>;
  }
  impl SendStream {
      pub fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Result<Option<VarInt>, StoppedError>>;
  }
  ```

- **B: `&self, &kio::Waiter`**, the convention in `rs/AGENTS.md`. Leak-free,
  since the caller owns the waiter, and the handles stay `&self`. But kio
  enters the driver's public signatures, so a kio break is a driver break,
  and every `cx`-based caller (moq-net's traits) holds a `Park` anyway.
- **C: a caller-owned `Wait` token** (`poll_closed(&self, cx, &mut Wait)`),
  slab-backed, generation-checked, and released on drop. Leak-free and
  `&self`, but a second waiter primitive beside kio's, with its own slab and
  tests to carry.
- **D: a stateless shared list.** Rejected: the leak above.

Recommended mechanism: swap the `Notify`s, and the `stopped` map's entries,
for `kio::WaiterList`s woken where the `Notify`s are signalled today, under
the connection lock, and make the existing async forms thin wrappers. They can
keep `&self` by owning their waiter (`kio::wait`), so no caller changes. Keep
the existing guarantees, such as draining accepted streams and datagrams
from a closed connection before its error. The other `Notify` waits
(`handshake_confirmed`, `Endpoint::wait_idle`) may move to the same lists,
but get no poll form until a caller needs one.

Maintainer question: A and B add kio as a direct dependency of the `driver`
feature. kio is already in the driver-only tree through `moq-quic`'s
moq-clock, which #6132 accepted (its decision 4), so no crate is added; B
also makes it public API. Recommended: A.

Lands after #6196, which rewrites the `stopped` waiter map. It blocks the
driver half of [poll_acked](/quest/m2/quic-ack-hook.md).

The acceptance test is the measured leak: 1000 waiters that poll once and are
dropped unresolved leave no live registration behind, checked before the
condition is ever signalled, so release on drop is what passes it, not the
next wake. Also, on paused time: two handles waiting on each condition both
wake; one handle polled 100 times holds one live registration; a clone's poll
leaves the original's registration in place. Report the public API: new poll
methods on `Connection`, `SendStream` and `Endpoint`, with the async forms
unchanged.

## Related

- [Drop web-transport-trait](/quest/m2/drop-web-transport-trait.md) - its native QUIC `poll::Session` calls these
- [poll_acked on moq-net's send stream](/quest/m2/quic-ack-hook.md) - adds `SendStream::poll_acked` in this shape
