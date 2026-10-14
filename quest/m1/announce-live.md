# [M] An announce consumer knows when it has caught up

## Goal

The Rust and JS announce consumers expose when a session's initial announce
snapshot has arrived. A consumer can tell "not announced" from "not heard
yet": a player shows offline instead of loading, and lazy discovery can hand
a watch from a broad announce request to a narrower one only once the
narrower one has caught up.

Non-goals: global completeness across a cluster, and any new wire format.
Every version that can carry the boundary already does.

## Plan

Raised 2026-10-13 on [#5466](https://github.com/moq-dev/moq/pull/5466).
Hand-over between a broad and a narrower announce request had two gaps when
the narrower request answered slowly, so #5466 keeps the broad request open
while anything beneath it is watched. The maintainer suggested this signal
as what would make a narrower hand-over safe.

The boundary is already on the wire, and the subscribers drop it:

- lite-01/02: the initial set is the single ANNOUNCE_INIT message.
- lite-05+, including lite-07: ANNOUNCE_OK's `Active Count` is the number of
  ANNOUNCE_START messages in the initial set. Publishers fill it from their
  current view; the Rust lite subscriber reads and drops it.
- IETF: the MoQ Active Count extension
  (`drafts/draft-lcurley-moq-active-count.md`) puts the count on the
  REQUEST_OK answering SUBSCRIBE_NAMESPACE. The Rust IETF subscriber checks
  it and drops it. JS only decodes the parameter: it neither declares the
  setup option nor sends the count.
- lite-03/04, and IETF without the extension (always on draft-14/15): no
  boundary. lite-03/04 have no OK either, and an unscoped draft-14/15 peer
  without MoQ Solicit is sent no request at all.

Decided with the maintainer, 2026-10-13:

- ✅ The boundary plus a public API. Rejected: wire only; not now. The
  maintainer chose a new lite-07 marker before the existing counts were
  found; this plan reuses them instead.
- ✅ m1. Rejected: m2.
- ✅ A relay reports caught up once it has sent its own current view of the
  prefix. It never waits for its upstream peers to catch up, so caught up
  means "caught up with the sender", not global completeness. Rejected:
  waiting for upstream markers. ANNOUNCE_OK's count already has this meaning.
- ✅ Where the session has no boundary, it reports caught up on the OK, as a
  best effort. Document the gap. Rejected: never caught up, which hangs
  consumers that wait on it. With MoQ Active Count negotiated, IETF waits
  for the counted NAMESPACE messages like lite does. Where there is no OK
  to wait on (lite-03/04, the unsolicited draft-14/15 case), it reports
  caught up once the request is sent, or at once when none is.
- Open, for the implementer to bring back with a recommendation: the API
  shape. The maintainer found an `Event::Live` enum variant ugly. The
  candidate is separate `poll_next` (route events only) and `poll_live`
  (ready once, then stays ready), with JS mirroring as `announced.live`.
  To avoid a race, the consumer is caught up only after it has drained
  every snapshot event before the boundary. Name the method by role.
  The shape must also say what caught up means for a consumer that spans
  sources (a reconnecting or pooled connection, an origin fed by several
  sessions or prefix requests): reset per source generation, or wait for
  every current source. "Ready once, then stays ready" is wrong after a
  reconnect. Readiness is a watermark over the coalesced queue, since an
  unseen start and end cancel out, and the origin keeps it so a consumer
  created after the boundary is caught up once it drains its replay.

Work:

- Rust `rs/moq-net` and JS `js/net`: the lite and IETF subscribers keep the
  count and mark the point where the initial set has been read, and the
  origin's announce consumers expose it. A counted snapshot cut short, by
  another message or the stream ending, is a protocol violation. Mirror
  names across languages.
- JS MoQ Active Count: declare the setup option, record the negotiation,
  validate the parameter against it (and refuse it on any REQUEST_OK but
  SUBSCRIBE_NAMESPACE's), and send the count as a publisher, so JS IETF
  sessions get the exact boundary too.
- `doc/concept`: what caught up means to a subscriber, including the
  best-effort versions.
- A benchmark sweeping announce streams and announce consumers, so
  propagating the boundary costs the touched path, not the table.
- Use it in lazy discovery: once both halves have landed, a narrower
  request may take over from a broad one after it reports caught up. Only
  an exact boundary qualifies; a best-effort session keeps the broad
  request. Do that here or in a follow-up, as the implementer judges.
- Regressions with mocked time: caught up after the counted snapshot and
  never before an earlier event is read; a count of 0 is caught up at once;
  a snapshot cut short is refused; a start and end that cancel before the
  poll still reach caught up; a consumer created after the boundary is
  caught up after its replay;
  a relay reports caught up without waiting on a silent upstream; IETF
  waits for the counted NAMESPACE messages, or reports on the OK without
  the extension; a session with no OK still reports caught up; a reconnect
  and a second feed follow the chosen multi-source rule. Run
  `just test interop --all`.

Public API: new caught-up surface on Rust and JS announce consumers. Wire:
no new format or draft change; JS adopts the existing MoQ Active Count
extension.

## Related

- [Lazy discovery](/quest/m1/lazy-discovery/README.md) - its broad-request hold is what this lets a narrower request replace
