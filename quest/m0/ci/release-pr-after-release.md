# [XS] A release merge opens no stale release PR

## Goal

Merging a release-plz release PR into `release` publishes that release and
opens no new release PR for the versions it just shipped. A new release PR
appears only when `release` holds unreleased changes. Done when a release
merge doesn't produce a stale release PR.

## Plan

Planned 2026-11-03 (maintainer, `/quest-plan`). The
[release run](https://github.com/moq-dev/moq/actions/runs/41704418718/job/126155827712)
from [moq-dev/moq#7651](https://github.com/moq-dev/moq/pull/7651) ran
`release-plz release-pr` before `release-plz release`, so it opened
[moq-dev/moq#7750](https://github.com/moq-dev/moq/pull/7750) for moq-tokio
0.19.33 four minutes before publishing it.

The fix is deployed: `just rs release` now publishes first, then proposes.

- [moq-dev/moq#7818](https://github.com/moq-dev/moq/pull/7818) landed it on
  `main` on 2026-11-04.
- [moq-dev/moq#7842](https://github.com/moq-dev/moq/pull/7842) backported only
  the recipe to `release` the same day.
- The backport merge's own
  [Release RS run](https://github.com/moq-dev/moq/actions/runs/41857741087)
  ran `release` (every crate already published) before `release-pr` (every
  crate up to date) and opened no PR. Its merge changed no crate, so it
  exercised the order but not a publishing release.

What remains is evidence from a release merge that publishes.

## Waiting

- moq-dev/moq: the next approved release-plz release PR merges into `release`;
  agents never merge into `release`. Check that merge's Release RS log: every
  `release-plz release` publish finishes before `release-plz release-pr` starts,
  and the run opens no release PR proposing the versions it just published.
  If so, delete this quest; if not, rewrite its Plan around that run.

Public API: none. Wire: none.
