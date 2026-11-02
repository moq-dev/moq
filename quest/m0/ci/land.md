# [M] Land main through GitHub's merge queue with an async fixer

## Goal

`main` changes only through GitHub's merge queue, one PR at a time. Each
`merge_group` build runs `just ci` on the host's self-hosted runner (label
`moq-queue`), and the queue merges exactly the tree it tested. When the
queue drops a PR for a failure or a conflict, moq-dev/quest's queue watcher
on the host runs the Codex land fixer in the background and re-enqueues the
PR. Only a decision the fixer can't make reaches a person. The host
`quest land` gate stops landing moq. Merged PRs per hour on a busy day match
or beat both baselines below.

## Plan

Decided 2026-11-01 (maintainer, /quest-plan), reversing the 2026-10-26
choice of a batched gate:

- ✅ Serialize through GitHub's merge queue again. The 2026-10-26 reason to
  leave it, conflicts GitHub cannot resolve (71% of 246 removals touched
  `quest/`), is now absorbed by the async fixer, which resolves them with the
  quest merge driver and re-enqueues. Batching and pre-checks are dropped:
  on moq a useful pre-check must compile, which is most of a check.
- ✅ moq moves first; moq-dev/quest, moq.pro, rust2 and rind stay on the
  host gate until moq proves this.
- ✅ `merge_group` builds run on the self-hosted runner on nixos (warm caches,
  no GitHub minutes), with build concurrency 1. Try 2 only with measured data.
- ✅ Host-gate work is frozen apart from what this switch needs.
- The watcher (re-enqueue at the back of the queue, unlimited attempts while
  each fix makes progress, flaky re-enqueues, fixing a pick GitHub refuses to
  enqueue) is planned in moq-dev/quest's m0 queue-watcher epic; moq waits on
  it before the cutover.
- Superseded designs are in git history: the 2026-10-26 three-phase
  `land.yml` and the 2026-10-30 host daemon (`quest land serve`).

Baselines:

- GitHub's merge queue, measured 2026-10-26 over 2026-10-23 to 10-26 (414
  queue merges, 76.7 h):

  - Throughput: 5.5 merged PRs per hour overall, and a median of 6.0 in the
    hours the queue never emptied. One PR at a time caps it at about 11.
  - Latency: from the final enqueue to merged, a median of 87 minutes and a
    p90 of 162. Waiting behind other entries is 98% of that. GitHub adds about
    1 minute per merge, and a job is a median of 4.7 minutes (p90 15.1).
  - Depth: a new entry found a median of 18 entries ahead of it.
  - Failures: 60 of about 470 runs failed (12.7%):
    - 25 semantic conflicts with PRs that landed ahead. A PR's base was a
      median of 22 commits behind the queue's.
    - 10 flaky tests, since fixed or tracked in the
      [flake epic](/quest/m0/test-flakes-2/README.md).
    - 8 lint failures and 9 failures from the PR's own tests or compile
      errors. A local `just ci` would have caught about 14 of these 17.
    - 8 infra failures, from mbx cache eviction before
      [moq-dev/moq#6709](https://github.com/moq-dev/moq/pull/6709).
  - Conflicts: 246 merge-conflict removals. 71% touched `quest/` and 41
    touched a `MIGRATE.md` Unreleased section.

  A simulation of one-runner batching with bisection gave 1.25 merged PRs per
  CI run at a 12.7% failure rate (best batch near 4), against 0.88 serial. At
  5% it gave about 2.4. The failure rate is the lever, which is why the flake
  epic moved to m0 and migration notes stay in PR Impact sections until the
  [release migration audit](/quest/m2/release/migrate-audit.md).

- The host gate, measured 2026-11-01: about 4-6 merged PRs per hour at a 23%
  per-PR failure rate, with batches shrinking to one PR and 10-17 minute
  checks (moq-dev/quest#376's measurements).

Remaining:

- **Ruleset.** `main` requires the `Queue` context, which
  `.github/workflows/queue.yml` reports from its `merge_group` build on
  `[self-hosted, moq-queue]` (skipped on pull requests, so a PR can enter
  the queue), and the merge queue (squash, build concurrency 1, merge limit
  1). It drops the unreported `quest land` requirement and keeps only the
  administrator bypass.
- **Host.** The host config stops serving moq from `quest land`
  (`services.quest-land.repositories.moq.enable = false`) at the cutover,
  while the watcher and fixer keep serving moq.
- **Dependabot and picks** enqueue through the merge queue like any other PR.
- Open since 2026-10-29: `APP_CLIENT_ID` and `APP_PRIVATE_KEY` are moq-dev
  organization secrets visible to this repository, so any workflow on a
  pushed branch can read the moq-bot key. The maintainer deletes the
  organization copies; `bot` and `publish` already hold the key.
- **Live evidence.** After the cutover, measure a busy day's merged PRs per
  hour and enqueue-to-merged latency against both baselines.

Public API: none. Wire: none.
