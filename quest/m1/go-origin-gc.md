# [XS] Go cancel test keeps its origin

## Goal

`TestRequestBroadcastCancelKeepsTheOrigin` in `go/wrapper/moq_test.go` passes
under load, not only alone (30/30 in isolation, `MoqError: Closed` under a
loaded run). Fixed at the cause, with no retry or longer timeout.

## Plan

Likely cause, found by reading and not yet reproduced: the test never touches
`origin` after `origin.Dynamic(...)` and `origin.Consume()`, so Go may collect
it mid-test. The generated finalizer then drops the only `MoqOriginProducer`,
the origin's driver finishes once every producer handle is gone
(`origin::Driver::poll` in `rs/moq-net/src/model/origin.rs`), and the next call
on the consumer or the dynamic handle gets `Error::Closed`. The collector runs
more often under load, which fits.

- Reproduce first: a `runtime.GC()` right after `cancel()`, or `GOGC=1`,
  should fail it every time.
- Keep the producer reachable to the end of the test (`runtime.KeepAlive`, as
  the file already does for `pending`), and audit the other `go/wrapper` tests
  that hold an `OriginProducer` only for setup.
- Users hit the same trap: a Go `OriginProducer` has no `Close`, so its origin
  ends whenever the collector reaches it. Say so in its doc comment and
  `doc/lib/go`. Whether consumers should keep their producer alive, or the
  wrapper should gain an explicit `Close`, is an API call: propose it to the
  maintainer rather than ship it here.

Public API: none unless the maintainer picks an API change. Wire: none.
