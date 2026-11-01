# [XL] Resume as producer replacement on one track

## Goal

A route change under a front replaces the producer writing into the
front's one logical track state, instead of every reader merging route
copies through its own cursors. Readers become plain cursors, the
`Inner::Resume` path and its per-reader copy bookkeeping are deleted in Rust
and `js/net`, and a benchmark swept over readers and route flaps shows the
cost no longer grows with readers times copies.

## Plan

From the 2026-10-24 pre-cut audit at 151a529670. Internal only: no public API
or wire change, so it can ship after the cut. The audit recommended after
the cut, not before: a rewrite now would discard the large resume test
corpus right before release. Decided 2026-10-25 (maintainer): the
model test (`rs/moq-net/src/model/resume_model.rs`) came first and gated the
cut; this rewrite moves to m1 only if that test shows design-level bugs.
It found single missed checks
([moq-dev/moq#6947](https://github.com/moq-dev/moq/pull/6947)), so this
stays after the cut, with the model as its oracle.

**Why.** Main replaced release's splice design with per-reader route copies
in 0382d309f4 (2026-10-04). Since then about 30 of 58 commits to
`rs/moq-net/src/model/resume.rs` were fixes, rising to about 10 a week, and
production code grew from 1,004 to 1,989 lines (`track.rs` from 4,203 to
7,626, partly features). The fixes cluster on state one logical reader keeps
in several owners, which must be kept in agreement by hand:

- where the reader is (`Reader.groups/window`, `Copy.floor/from/resolved`,
  cursor bounds, `Delivered.floor`): fd59eb18a5, 9f1fc92fe3, 35fa44f6bf, 81d8a6313c, 137de79100
- what was handed out (logical and per-copy `delivered`, `Reader.newest/out`,
  the two-level peek): 323fda277f, bfbaa0b46d, 342445f251, 3602a8af9a
- whether a replaced copy is still needed (`Copy::retains` asking another
  layer's cursor): 9b1b81a496, 5e6c452305, 9bba359f6b, 2fbaa59428
- route failure versus content verdict: 39eacc807a, 109235732c, eb6ac57cb7, 5c31208409
- end agreement and budget/expiry: 25532eacc8, a602360c99, 097dc1ee62, f52848d634, b59fcefde3, e33fe24cc3

Every `track::Consumer`/`Subscriber` method has two implementations (26
`Inner::Resume` arms, 11 `serving()` delegations), so each feature (ranges,
anchors, fills, compression, largest, drops, start, live) is written twice
and drifts. `js/net/src/resume.ts` (1,786 lines) mirrors it, and the resume floor fix
[moq-dev/moq#6759](https://github.com/moq-dev/moq/pull/6759), the resumed JS drops fix
[moq-dev/moq#6758](https://github.com/moq-dev/moq/pull/6758), and the compress check
[moq-dev/moq#6460](https://github.com/moq-dev/moq/pull/6460) are each a place the two
paths diverged.

**Sketch.**

1. Route copies become successive writers into the logical `TrackState`,
   the only cache. Every writer is of the instance the subscription started
   on: a newer [publisher epoch](/doc/concept/moq-lite.md#publisher-epochs)
   never feeds it (the route rule in `rs/moq-net/AGENTS.md`), so its readers
   end with an error once their instance's routes are gone. Same-epoch routes
   carry identical content, so a later writer's duplicate sequence is
   dropped or kept as the slot's backup. The old writer keeps feeding below
   the new writer's first group and is released once the logical cache
   covers that span or the aggregate budget passes, judged once per track by
   `drifted`, not per reader.
2. The upstream request is already the union of readers' resume points
   (`Producer::subscription`), so per-reader copy cursors are local overhead.
3. Readers become plain cursors: delete `Inner::Resume`, `Reader`, `Copy`,
   `Retain`, `Candidate`, per-reader datagram dedupe (dedupe once on insert),
   and per-reader `learn_end`.
4. `Recover` attaches to the slot, not a handed-out group: continuing a
   group whose copy failed is a
   [flight](/quest/m1/unbounded-groups/flights.md) from the reader's frame on
   the new route, which hands over like any other flight.

Hard constraint from the splice era: two caches sharing one group handle
caused expiry bugs (f9b7982e01, 6c2b733bbb) and boundary churn (06bcadd06c,
b9398f7459, 68c73349a3). Session copies under a front must stop caching for
themselves and only feed the logical track.

Land it in Rust first, then mirror `js/net` unless
[JS-only @moq/net](/quest/m2/rs2ts-js.md) has replaced it. Extend
`benches/origin.rs` with the reader times flap sweep before starting, to
measure the change.

Public API: none. Wire: none.

## Required

- [Flights](/quest/m1/unbounded-groups/flights.md) - the paced copy and handover `Recover` folds into
- [Release cut](/quest/m2/release/cut.md) - wait for the release in flight to ship

## Related

- [A reserved group survives a route returning](/quest/m1/cut-audit/reserved-route-return.md) - a pre-cut fix this deletes
- [Recover reads its budget without a sync](/quest/m2/recover-budget.md) - a per-frame cost this deletes
