# [S] io_uring socket teardown releases its resources

## Goal

Opening and dropping `moq-uring` UDP sockets on a long-lived worker never
exhausts or leaks ring resources: the 65,536th socket opens like the first,
and a dropped socket's receive is always cancelled or the failure surfaces.

## Plan

Neither is reachable in the relay, which binds one socket per worker once;
both are reachable by an embedder that opens a socket per dial.

- Buffer-group ids: `next_bgid` (`rs/moq-uring/src/udp.rs`) is a per-worker
  `u16` allocated with `expect`, before the multishot check, never returned
  on failed registration or socket drop, so the 65,536th `Handle::udp`
  panics. Allocate only for multishot sockets and recycle an id only once
  the kernel has released its buffer ring. Cover failed construction,
  churn past the id space, and reuse.
- `Socket::drop` discards a failed cancel (`let _ = shared.cancel(key)`),
  leaving an idle multishot receive holding the fd and pool until worker
  teardown. Only an `io_uring_enter` error reaches it. `EINTR` is a
  signal, not a broken ring, but the non-deadline `push_inner` path
  surfaces it today, so retry it there; anything else means the ring itself
  is broken. `drop` cannot return it, so record it on `Shared`
  and fail the worker's next turn with it, the same as any other ring
  failure, so every handle on the worker fails. Test by injecting the push
  failure.

Decided 2026-10-22 from the io_uring audit, split from the QUIC handle
lifecycle work in review since neither is a QUIC handle and neither gates
packaging.

Public API: none. Wire: none.
