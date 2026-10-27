# [L] moq-net's transport traits poll on kio waiters

## Goal

moq-net's `transport::poll` traits take `&kio::Waiter` instead of
`&mut Context`. `Session` loses its `Clone` bound and every operation on it
takes `&self`: accept, open, datagrams, `poll_closed`, `close`, and stats.
On streams, ordered I/O (`poll_write`, `poll_lease`, `poll_read_chunk`) stays
`&mut self`, observations (`poll_closed`, and `poll_acked` once
[poll_acked](/quest/m2/quic-ack-hook.md) adds it) take `&self`, and terminal
operations keep their receivers. moq-net shares one `Arc<S>` where it cloned
the session, and every implementation follows.

## Plan

Decided with the maintainer on 2026-10-25:

- **`&self` because the caller owns the waiter.** quinn's `&mut self` waits
  exist because a `cx` waker has to be stored per caller: a `Park` in each
  handle, so a handle per waiting task, so `Clone`. `&self` with `cx` would
  push wakers into shared lists that outlive their callers: a 2026-10-23
  trial of that shape kept all 1000 abandoned `poll_closed` waiters alive. With `&kio::Waiter`, the
  `rs/AGENTS.md` convention, `&self` is leak-free. Async wrappers then hold
  no exclusive borrow across an await.
- **No `Clone`.** The trait's "independent progress on each handle" contract
  becomes "an operation keeps no per-caller progress between polls". moq-net
  holds `Arc<S>` at its clone sites (client, server, auth, stats, the IETF
  adapter), so an implementation no longer has to be
  a cheap handle.
- **Rejected: `&mut self` without `Clone`.** moq-net opens streams from many
  tasks at once, so every open would have to go through the session's owner
  task, or callers would wrap the session in `Arc<Mutex<_>>` themselves.
- **A mutex is acceptable for now.** `&self` commits an implementation to
  neither a `Mutex` nor an `Arc`: the tokio driver needs a lock because its
  handles cross threads, while moq-uring serves the same calls from an
  `Rc<RefCell>` per core. Removing either is
  [borrow-free state](/quest/m2/perf/quic-borrow-free.md)'s question.
- **Before [native WebTransport](/quest/m1/quic-api/native-web.md)**, so the
  native QUIC impls forward to the drivers' `&self, &kio::Waiter` forms in
  one line, with no `Park` bridge.

Implementations to update: `transport::memory` and `transport::boxed`, the
IETF `ControlStreamAdapter`, qmux's `Session`, moq-tokio's `transport`
adapters (`owned.rs`), moq-uring's `transport::{adapter, local}`, the ffi and
wasm adapters, moq-mux's `Gated`, and moq-net's test sessions. Each one
that keeps per-handle progress across polls today moves it into shared state
or into the handle it returns; list them in the PR.

Public API: breaking for every `transport::poll` implementor and for callers
that relied on `Session: Clone`. The PR's Impact migration note includes a
before/after impl. Wire: none.

## Related

- [Poll forms](/quest/m1/quic-api/poll.md) - the QUIC drivers' matching receivers
- [Drop web-transport-trait](/quest/m1/drop-web-transport-trait.md) - its iroh, wasm, and ffi impls land on this shape
- [Generated async wrappers](/quest/m1/poll-first/async-feature.md) - generates the `&self` async forms from these polls
