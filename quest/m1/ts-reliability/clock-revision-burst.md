# [S] A TS export never loses a queued clock revision

## Goal

The TS export lays every TDT/TOT revision it has queued on the subscription
at its stamped slot on the media grid, however many arrive before media
reaches the first one, and whatever the SI track's max-age. Done when either
a test proves that a burst of queued revisions all reach their stamped slots,
or the loss is shown to be a fixture artifact and the fixture is fixed.

## Plan

Investigated 2026-11-04 in [moq-dev/moq#7857](https://github.com/moq-dev/moq/pull/7857).
The paired test `queued_clock_revisions_follow_the_exporters_freshness_budgets`
in `rs/moq-mux/src/container/ts/export_test.rs` queues three revisions without
polling between writes, then checks full section bytes and media slots 3, 4,
and 5 seconds independently in both exporters, for TDT and TOT.

An unlimited SI publisher and a six-second export delay preserve all three.
A six-second publisher age preserves the same burst with that delay. With
zero export delay, only revision 3 survives despite an unlimited publisher.
With 500 ms export delay, revisions 2 and 3 survive. A 500 ms publisher age
also loses revision 1 despite a six-second export delay. These deterministic
controls run in the normal CI selection, with paused time and no added skips.

The loss is the subscription's freshness bound, not replacement of the held
revision. `SiTrack::new` receives the export delay, which `SiTrack::poll` asks
for as the subscription's maximum delay; moq-net clamps that to the published
max-age. The single held future revision leaves later revisions on this
bounded subscription until media reaches it.

The historical `export_twice` fixture opens zero-delay exporters, so an
unlimited publisher alone never promised burst history. Its `timed()` helper
now inherits a zero publisher age on main after
[moq-dev/moq#7729](https://github.com/moq-dev/moq/pull/7729); the new burst test
sets age explicitly with the importer's `si::track_info`. Existing cadence
tests retain their original age. The budgeted pair drains concurrently so
one drain cannot make the other exporter's media late. Placement is checked
from the output PES PTS because video send-ahead uses earlier PCR slices.

No production code, snapshot-table semantics, retention, or budget rules have
changed. The unqualified Goal conflicts with those bounds. The proof covers
bursts inside them, so completing the quest requires the decision below.

Public API and output TS: unchanged in the investigation PR.

## Waiting

- Maintainer: choose the clock-burst guarantee before this quest is completed.
  1. Recommended: preserve existing freshness bounds and qualify the Goal to
     require every revision that remains within the requested delay and
     publisher max-age. The paired proof covers this contract; the zero-delay
     fixture keeps its cadence role.
  2. Require every future-stamped revision regardless of those bounds. Settle
     a bounded ownership or backpressure design first; retaining an arbitrary
     burst outside the subscription's freshness limits changes the contract.
  An agent cannot choose between the Goal and the settled freshness bounds.
  Check that the chosen contract is recorded here before removing this wait
  or changing the exporter.
