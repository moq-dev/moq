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
  in the generators, so moq-ffi exports a poll once. The Go wrapper's bridge
  goroutine can go for these reads.
- **Go's cancel contract.** `go/wrapper/doc.go` promises that cancelling a
  `RecvGroup` or `ReadFrame` context ends the stream. The protocol makes a
  per-read cancel safe. Decide with the maintainer whether a cancel now
  ends only that read or still ends the stream. If it changes, update the
  doc, the tests, and the PR's API report; the other wrappers face the same
  question.
- **Latch wakes.** A wake that lands after a poll returns `Pending` but
  before the native future parks must not be lost. The spike's Go waker was a
  buffered channel, and its Python one swapped in a fresh future before each
  poll. Each backend needs an equivalent handshake and a test that forces
  that interleaving.
- **Prove no-loss per backend.** A sync poll only moves the race into the
  generated glue: if it polls an item out and then sees its future cancelled,
  the item is gone. Each backend that can cancel must force that window in a
  test. Either the result is delivered or the next read gets the item. The
  spike's Python and Go cancel probes are a starting point.
- **Concurrent pollers on one handle share one waker.** Pick a semantic and
  test it: either reject a second concurrent read, or wake every poller.
- Measure every generated wrapper (Go, Python, Kotlin, Swift, Dart, C++);
  the spike covered only Go and Python. Sweep
  publishers as well as subscribers, since the generator's callback-handle map
  is shared process-wide.

Public API: these reads gain a poll protocol in every binding; their async
signatures stay where a language keeps both. Go's read cancel may change from
terminal to per-read. Wire: none.

## Required

- [Hard-fork the binding generator](/quest/m2/uniffi-fork.md) - the generators that emit the protocol
- [Poll-native async](/quest/m2/ffi-poll-migrate.md) - the kio poll each export wraps, with `ffi::Task` gone

## Related

- [Kotlin calls](/quest/m2/kt-ffi-calls.md) - the Kotlin call overhead this protocol also pays
