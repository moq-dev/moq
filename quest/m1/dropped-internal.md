# [M] No made-up Dropped

## Goal

`Error::Dropped` only ever means an application dropped a producer without
finishing or aborting it. No moq-net code path, or any other first-party
crate, uses `Dropped` as a stand-in for a cause it could name. Rust only:
`js/net` has no equivalent.

Non-goals: the refcount fallback itself stays (readers of a producer the app
dropped still see `Dropped`), as do explicit drop paths for handles the app
owns (an unanswered `group::Request` or `track::Request`, the last `Dynamic`
handler, a `frame::Raw` with unwritten bytes). Fronts, resume, and the wire
catch-alls belong to [Dropped sources](/quest/m1/dropped-sources.md).

## Plan

Decided with the maintainer on 2026-10-13:

- Fix internal sites that put `Dropped` where they could give the real cause,
  as inventoried that day. Line numbers will drift, so re-run the inventory
  (`grep -rn "Dropped" rs/`) rather than trusting this list:
  - `lite/subscriber.rs`: `end_reason()` falling back to `Dropped` when no
    session end was recorded, and the settle path when `closed` fires.
  - `ietf/subscriber.rs`: the alias table closing while resolving, and the
    fill / group-fetch slots closing when the subscription goes away.
  - `model/origin.rs`: teardown rejecting parked front and route requests
    (an existing `Dropped` -> `Closed` remap shows the intent), and a copy
    closed while its completion is still pending.
  - `Action::Forget` in `model/origin.rs` ends with `Cancel`; we chose to stop.
  - `moq import` failures: the stdin decode error path in
    `rs/moq-cli/src/publish.rs` and `Programs::abort` in
    `rs/moq-mux/src/container/ts/programs.rs` drop their catalog producers,
    so catalog subscribers see `Dropped` instead of the import's error.
    `catalog::Producer` has no public abort yet, and they need one.
  - The `moq-transcode` rung whose source closes while idle passes on the
    source's own end, or `Cancel` if it ended cleanly.
- Use existing variants; no new public variant.
- Error arms that swallow a cause (the lite subscriber's "our own producer is
  alive" arms) propagate the error they received. They are reachable: a live
  clone can abort with a real cause, such as `ProtocolViolation` for a group
  past the declared end.
- Only states proven unreachable (perhaps an IETF alias or fill `write()` on a
  closed table) return the nearest real cause plus `debug_assert!(false, ...)`,
  so tests catch a broken invariant without panicking a release relay. Prove
  it first; never assert on a path peer input can reach.
- Audit the consumers that match `Dropped` (`moq-c` publish, `moq-ffi`
  demand, `moq-hls` rendition export, the TS follower, audio capture, video
  producer). Each either keeps the arm with a comment that it handles the
  app dropping its own producer, or switches to the error it was standing in
  for. Coordinate the TS follower with Dropped sources, which stops it
  matching errors at all.
- Guard with tests only: one regression test per fixed site asserting the
  specific error. No runtime check on the refcount fallback.

Public API: adds `abort` to `moq_mux::catalog::Producer` for the import
failure paths; error values consumers observe change. Wire: no new wire
format or codes, though replacing `Dropped` with `Cancel` or `Closed` can
change which existing codes are emitted.

## Related

- [Dropped sources](/quest/m1/dropped-sources.md) - the same rule for track ends that reach the wire, fronts, and resume
