# [M] An announce consumer knows when it has caught up

## Goal

A moq-lite-07 announce stream marks the end of its initial snapshot, and the
Rust and JS announce consumers expose that point. A consumer can tell "not
announced" from "not heard yet": a player shows offline instead of loading,
and lazy discovery can hand a watch from a broad announce request to a
narrower one only once the narrower one has caught up.

Non-goals: global completeness across a cluster, and any change to
lite-05/06 or the published IETF drafts.

## Plan

Raised 2026-10-13 on [#5466](https://github.com/moq-dev/moq/pull/5466).
Hand-over between a broad and a narrower announce request had two gaps when
the narrower request answered slowly, so #5466 keeps the broad request open
while anything beneath it is watched. The maintainer suggested this signal
as what would make a narrower hand-over safe. lite-01/02's ANNOUNCE_INIT
carried a snapshot boundary; later versions dropped it.

Decided with the maintainer, 2026-10-13:

- ✅ A lite-07 wire marker plus a public API. Rejected: wire only; not now.
- ✅ m1, so it lands in lite-07 before the cut. Rejected: m2.
- ✅ A relay sends the marker once it has sent its own current view of the
  prefix. It never waits for its upstream peers to catch up, so the marker
  means "caught up with the sender", not global completeness. Rejected:
  waiting for upstream markers.
- ✅ An IETF session reports caught up on SUBSCRIBE_NAMESPACE_OK, as a best
  effort, since the drafts have no end-of-snapshot marker. Document the
  gap. Rejected: never caught up on IETF, which hangs consumers that wait
  on it.
- Open, for the implementer to bring back with a recommendation: the API
  shape. The maintainer found an `Event::Live` enum variant ugly. The
  candidate is separate `poll_next` (route events only) and `poll_live`
  (ready once, then stays ready), with JS mirroring as `announced.live`.
  To avoid a race, the consumer is caught up only after it has drained
  every snapshot event sent before the marker. Name the method by role.

Work:

- Wire: one lite-07 marker after the initial burst on each announce stream.
  Pick the encoding (a message type or a field) and update
  `drafts/draft-lcurley-moq-lite.md` and its changelog. Validate with
  `just drafts check`.
- Rust `rs/moq-net` and JS `js/net`: the publisher sends the marker, the
  subscriber surfaces it, and the origin's announce consumers expose it.
  Mirror names across languages.
- Use it in lazy discovery: once both halves have landed, a narrower
  request may take over from a broad one after it reports caught up. Do
  that here or in a follow-up, as the implementer judges.
- Regressions with mocked time: caught up after the snapshot and never
  before an earlier event is read; a relay reports caught up without
  waiting on a silent upstream; IETF reports it on OK. Run
  `just test interop --all`.

Public API: new caught-up surface on Rust and JS announce consumers. Wire:
lite-07-wip only, which is unpublished.

## Related

- [Lazy discovery](/quest/m1/lazy-discovery/README.md) - its broad-request hold is what this lets a narrower request replace
- [Finalize moq-lite-07](/quest/m1/lite07-finalize.md) - lite-07 carries the marker before the cut
