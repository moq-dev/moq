# [S] A TS export never loses a queued clock revision

## Goal

The TS export lays every TDT/TOT revision it has queued on the subscription
at its stamped slot on the media grid, however many arrive before media
reaches the first one, and whatever the SI track's max-age. Done when either
a test proves that a burst of queued revisions all reach their stamped slots,
or the loss is shown to be a fixture artifact and the fixture is fixed.

## Plan

Planned 2026-11-04 (maintainer, `/quest-plan`), from testing
[moq-dev/moq#7729](https://github.com/moq-dev/moq/pull/7729). While that PR
was tested, the existing two-exporter fixture in moq-mux's TS export tests
dropped a queued TDT/TOT revision even though the SI track's max-age was
unlimited. So the exporter may lose a burst of queued clock revisions for a
reason that has nothing to do with max-age. **This is unconfirmed, and may be
fixture-only.** #7729's body and review threads record no repro for it.
They cover only the max-age regression it fixed: a zero-age SI track skipped
a superseded revision at once, tested by
`a_superseded_si_revision_stays_readable_within_the_export_budget`.

Background:

- [moq-dev/moq#7779](https://github.com/moq-dev/moq/pull/7779) placed clock
  revisions on the stamped media grid. The export reads a clock table in
  order and holds one revision until media reaches its stamped slot, while
  later revisions wait on the subscription.
- [moq-dev/moq#7729](https://github.com/moq-dev/moq/pull/7729) gives the TS
  importer's SI tracks their catalog's media max-age, because the export's
  SI budget is capped by that age.
- The fixture is `export_twice` in
  `rs/moq-mux/src/container/ts/export_test.rs`, used by
  `late_join_matches_a_running_exporter`, its `_without_video` variant, and
  `a_late_clock_revision_remains_visible_between_exports`. Its clock tracks
  are created with `timed()`, which sets no max-age, so they are unlimited
  on main. Clock revisions arrive between the two exporters' drains, before
  their stamped media position.

Steps:

- Reproduce it first. Write a test that queues a burst of several TDT and TOT
  revisions, each stamped ahead of the media, on an unlimited SI track. Check
  that every revision appears at its slot, in order, in each exporter's
  output.
- If a revision is lost, find where: the single held revision being replaced
  rather than queued, a later revision read past while one is held, or the
  delay budget expiring on a revision behind the held one. Fix the exporter,
  and keep the burst test.
- If the exporter is correct, find what the fixture does wrong (for example
  the drain order, or a revision written after its slot already passed), fix
  `export_twice`, and record the cause here before deleting the quest.
- Don't change snapshot tables: they restate the latest value and may skip
  superseded ones.

Public API: none. Wire: the TS output gains any revision that was dropped.

Investigated 2026-11-04: `export_twice` opens zero-delay exporters, so their SI
subscriptions request zero superseded history even when the publisher's max-age
is unlimited. A three-revision burst on unlimited tracks reproduced only the
newest revision in both outputs. The new paired cadence test gives both readers
an explicit six-second budget: all three revisions reach media slots 3, 4, and
5 seconds, with their full section bytes intact, for both TDT and TOT. Its
zero-delay control still emits only the newest revision. Clock fixtures now
declare unlimited publisher age explicitly after the default-age change.
The omission is the fixture's no-history budget, not replacement of a held
revision. No exporter, snapshot-table, retention, public API, or TS-output
behavior changes are needed.
