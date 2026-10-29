# [L] Flights

## Goal

A reader whose frames a group copy has shed, such as a late joiner that wants
frame 0 of a long group or a reader that fell more than the 32 MB tail behind,
is served again when the publisher allows replay, without any frame being
fetched faster than its readers take it. Readers replaying the same part of a
group share one upstream fill, and a reader that catches up hands over to the
copy ahead, whose fill then stops. A track whose publisher has no
`track::Requests` handler keeps refresh-or-`Evicted` from
[Front eviction](/quest/m1/unbounded-groups/front-eviction.md).

## Plan

Decided 2026-10-28 (maintainer, after an audit of the plan and
[moq-dev/moq#6993](https://github.com/moq-dev/moq/pull/6993)). This reopens
2026-10-24's rejection of paced refill "trains" that merge, and replaces
#6993's unpaced exact-hole refills written into runs below the live log.
That rejection worried that a merged reader sits at the front, the first
victim; the 16 MB rule below answers it. #6993 measured about 2.7x the
fetch cost per frame for a hole longer than the pool, because an unpaced
refill sheds its own tail before its reader gets there; a paced flight
never does.

- A flight is a separate copy of a group: its own upstream fill from its
  first rider's frame into its own group copy, with its own 32 MB tail, paced
  by its fastest rider like the live edge
  ([Fastest-reader pacing](/quest/m1/unbounded-groups/fastest-reader.md)).
  The live subscription is the first flight. Copies are never stitched
  together: two caches sharing one group handle caused the expiry and
  boundary bugs [Resume redesign](/quest/m2/resume-redesign.md) records.
- Who serves a reader at a shed frame: a flight when the track has a
  `track::Requests` handler (the publisher's permission to replay), else a
  new group on tracks that opt into refresh, else `Evicted`. The origin's
  handler decides whether to serve old content; refresh is the fallback
  (decided 2026-10-24). Byte streams register no handler, so they never
  start a flight.
- Admission: a reader joins an existing flight only if that copy holds its
  frame within 16 MB, half the tail, of the flight's fastest rider, less one
  shedding unit (a log segment). Judge it against the worst-case front:
  the ahead window is capped at 16 MB
  ([Fastest-reader pacing](/quest/m1/unbounded-groups/fastest-reader.md)),
  so filling all outstanding credit still leaves an admitted reader held.
  Otherwise it starts its own flight, so it never boards at a front about to
  be shed.
- Handover: a rider whose frame the flight ahead holds within the same
  admission bound of that flight's fastest rider switches to that copy at the same frame, the way
  route failover's `Recover` continues a group from another copy. A
  flight's fill is cancelled when its last rider leaves. A merge costs at
  most 16 MB of duplicate transfer, and the budget never grows. Rejected:
  merging as soon as the copies touch, with the merged flight's budget
  growing toward 64 MB until its slow riders catch up.
- A rider that falls off its flight's tail is a reader at a shed frame: it
  starts or joins another flight by the same rules.
- Upstream: a relay's flight is a frame-offset fill (lite-06+ `Frame Start`)
  that the upstream serves from its own flights, recursively, until the
  origin's handler serves or refuses it. Legacy lite peers widen to frame 0.
  Over moq-transport a flight starts at an object boundary.
- Memory is 32 MB per flight, bounded overall by pool debt. Open, for the
  implementer to measure and bring to the maintainer: whether a group needs
  a cap on concurrent flights, and how a compressed flight that starts past
  frame 0 gets the decoder state its plain riders need.
- Tests through a two-relay chain: a late joiner of a group many times the
  tail reads it from frame 0 through a flight and hands over to the live
  copy, with memory near 32 MB per flight; a rider that just handed over
  survives the copy ahead filling its whole ahead window at once; joiners
  within 16 MB of each other share one upstream fill; a slow rider falls off and starts its own
  flight without slowing the fast one; no flight fetches a frame twice; a
  track with no handler refreshes.
- Benchmarks: a case swept over readers and how far apart they join.

Public API: none. Wire: none, beyond the existing frame-offset fills.

## Required

- [Fastest-reader pacing](/quest/m1/unbounded-groups/fastest-reader.md) - flights pace on their fastest rider the same way the live edge does

## Related

- [Refill evicted groups](/quest/m2/evicted-refill.md) - refills whole groups evicted before a reader took them
