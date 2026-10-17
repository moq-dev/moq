# [L] Foreign poll for hot reads

## Goal

Track and group reads (`recv_group`, `next_group`, `read_frame`, and
`recv_datagram`) reach every wrapper as a sync poll plus a waker registered once per
handle. The fork's generators build each language's native future around
them, so a ready read costs no event-loop round trip. A cancelled read never
loses a frame. Other async exports keep UniFFI's `RustFuture`.

## Plan

Decided with the maintainer on 2026-10-16, from
[the spike](https://github.com/moq-dev/moq/pull/5851) (Shape B′, prototype
on `spike/ffi-poll-prototype`). It measured, on a loaded host:

- Python: a ready `read_frame` took 7.4 µs against 24.5 µs for Shape A. Burst
  throughput was 1.7x Shape A's, with no loop wakeup per ready read.
- Go: a ready `read_frame` took 0.67 µs against 1.2. At 64 subscribers
  throughput was 1108 MB/s against 713, at 8.2 µs CPU per frame against 11.5.
- Cancel: Shape A and `Task` lose a frame whose `RustFuture` finished before
  the foreign side dropped it. Python asyncio and Go ctx cancels both hit
  this. A sync poll cannot, since the caller takes the item in the same call.

Things to carry over and check:

- **Register the waker, don't pass it per poll.** The spike's per-call
  `poll_x(waker)` registered a new callback handle in the generator's global
  map on every poll and freed it through another callback. That dropped Go
  at 64 subscribers to 89 MB/s. A handle's waker is set once. Decide how a
  new group inherits its track's waker, rather than paying an extra call per
  group.
- **Generated, not hand-written.** The per-language future glue (asyncio,
  Go channel and ctx, Kotlin and Swift continuations, Dart, C++, C) belongs
  in the generators, so moq-ffi exports a poll once. The Go wrapper's
  terminal cancel and its bridge goroutine can go for these reads.
- **Concurrent pollers on one handle share one waker.** Pick a semantic and
  test it: either reject a second concurrent read, or wake every poller.
- Measure Kotlin and Swift too; the spike covered only Go and Python.

Public API: these reads gain a poll protocol in every binding; their async
signatures stay where a language keeps both. Wire: none.

## Required

- [Hard-fork the binding generator](/quest/m2/uniffi-fork.md) - the generators that emit the protocol
- [Poll-native async](/quest/m2/ffi-poll-migrate.md) - the kio poll each export wraps, with `ffi::Task` gone

## Related

- [Kotlin calls](/quest/m2/kt-ffi-calls.md) - the Kotlin call overhead this protocol also pays
