# [S] Release resumes owed groups

## Goal

A `release` relay's resumed read gets every owed group or a drop for it,
never a hang. Exposure is reproduced; the source fix and a separately
authorized patch release remain.

## Plan

Decided 2026-11-03 (maintainer, /quest-plan of the follow-ups from
[moq-dev/moq#7708](https://github.com/moq-dev/moq/pull/7708)): milestone m0,
beside the other backports. The main fix has merged.

The exposure investigation is recorded in
[moq-dev/moq#7815](https://github.com/moq-dev/moq/pull/7815).
At `release` commit `da09919e75f285ed1fe0b45b35d34022ffbc14f2`
(moq-net 0.3.20), adapted paused-time session probes show:

- Cache group 0, leave until the publisher reports unused, publish groups
  1 through 3, then resume at group 0 with a budget covering the history.
  Every IETF draft from 14 through 22 returns group 3 instead of group 0.
  Applying release's local cursor cap `set_groups(0..1)` leaves the read
  waiting for group 0. All seven lite versions, including lite-07-wip,
  retain group 0 in both cases.
- The separate bounded `1..3` probe behind a fetched copy delivers groups
  1 and 2 on drafts 14 through 22. It does not establish that an initially
  bounded IETF request is accepted: release opens the subscription while
  querying metadata, before the reader's preferences reach it.
- Release's movable local cap intentionally parks at its boundary. Main's
  bounded EOF assertion is not applicable to this API.

The root differs from main's shared `live_floor`:
`origin::run_front` queries a source before splicing the reader's demand.
The IETF subscriber snapshots the empty demand as a live join and records
SUBSCRIBE_OK's Largest as its start. `resume::Subscriber::poll_activate`
then discards the whole warm segment when that start exceeds its edge,
without judging the resumed reader's budget.

The remaining work is a hand-port that preserves the published API and
finite-budget behavior. Do not merely remove the warm-cache gate: main's
fix distinguishes unlimited from finite history, while release exposes a
concrete `max_age: Duration` and has no unlimited option. Forwarding the
actual demand before IETF setup is the recommended direction; it needs
coverage of metadata-only lookups, fetched copies, and concurrent readers.
Keep the original cache-loss and bounded-resume matrices for drafts 14 to
22 and every lite version.

Keep this separate from [the joined-head FETCH backport](/quest/m0/backport-fill-head-race.md)
and [moq-dev/moq#7680](https://github.com/moq-dev/moq/pull/7680).
Its queued-miss grace correction is a different path.

After a reviewed source backport lands, use the normal patch-release
process only with explicit release authorization. Do not bump versions,
tag, or publish as part of the implementation attempt. Verify the
published source includes the fix before deleting this quest.

Public API: preserve the published surface. Wire: no format change expected.

## Waiting

- Maintainer: choose whether to continue with a demand-setup hand-port (recommended), preserving finite-budget cache behavior, or explicitly permit a change to that behavior. Removing the warm-cache gate alone changes which stale cache finite readers receive; main's unlimited-only exception cannot be copied into release's older API.
- Maintainer: authorize the separate `moq-net` patch release after the source fix is reviewed and landed; retain this requirement until the published source is verified. Release cuts and version changes require their own authorization.
