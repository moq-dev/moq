# [S] A release re-measures its planned wire-compat breaks

## Goal

Publishing a release that changes a package named in
`test/interop/compat/planned-breaks.json` no longer turns the scheduled
Interop job red. As part of the release, the maintainer re-measures the
released wire compatibility against the versions just published, with
planned breaks run rather than skipped. Each planned break is then re-pinned
with a fresh measurement, or deleted because it no longer fails. No entry is
ever re-pinned without being re-measured. Scheduled runs keep skipping
planned breaks as they do today.

## Plan

After the 0.17 cut, the scheduled job's "Released wire compatibility" step
failed with `stale planned break: moq-lite-07-wip, moq-cli changed from 0.14.4
to 0.14.5`. #5570 fixed it by hand: empty the file, run the matrix, re-pin
what still fails. That staleness check is deliberate. A break pins exact
releases so that a new release forces a review. The gap is that nothing in
the release procedure does the review, so `main` finds out a night later.

Root problem: the measurement the check asks for is a manual,
undocumented step. Make it one command and part of the release procedure.

- Give the harness a re-measure mode. It runs every cell, planned breaks
  included, and reports per entry whether it still fails and which cells do.
  It also reports an entry that no longer fails, so an obsolete break is
  deleted rather than carried. Today `just test wire-compat` always skips
  the cells a planned break covers, so running it as-is at a release
  measures nothing about the breaks.
- Have the stale-entry error name that command, so the next person it hits
  knows the fix.
- Add the step to `CONTRIBUTING.md`'s Releases section, after publication.
  It covers patch releases from `release`, not only the breaking cut. Keep
  the planned-break rules in `test/interop/README.md` consistent with it.

Open question for the maintainer: how much of the step is automated.

1. (recommended) The re-measure mode plus the documented manual step. The
   maintainer reviews the measured result and edits the reasons. That review
   is the point of the check.
2. A workflow that runs on publication, runs the re-measure mode, and opens
   a PR with the measured pins for review. It is less manual, but it needs
   a PR token that triggers CI, and its reasons still need a human edit.

Until this lands, each release repeats #5570's manual re-measure.

`catalog-archive` goes once an `@moq/hang` release carries the tolerant
catalog parser. It is backported on `release` (#5520) but not yet published to
npm. The re-measure after that release shows the entry passing. Open question:
whether the next 0.17 patch in [Draft-22 media on 0.17](/quest/m0/release-22/README.md)
also publishes `@moq/hang`. Recommended: yes, so the entry can be deleted at
that cut.

Public API: none. Wire: none.

## Related

- [Cut and publish](/quest/m2/release/cut.md) - the breaking cut, which follows the same procedure
- [Draft-22 media on 0.17](/quest/m0/release-22/README.md) - the next 0.17 patch, the first release this applies to
