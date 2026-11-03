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
- The watcher and fixer are implemented in moq-dev/quest. Historical
  cutover evidence and the remaining verification are below.
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
  checks ([moq-dev/quest#376](https://github.com/moq-dev/quest/pull/376)).

Cutover recheck, 2026-11-03 (read-only):

- **Ruleset verified.** REST `GET /repos/moq-dev/moq/rulesets/2420853`
  reports active enforcement, only `Queue` required (GitHub Actions
  integration 15368), and a `SQUASH` merge queue with
  `max_entries_to_build=1` and `max_entries_to_merge=1`. Only the
  administrator role bypass remains; `quest land` is no longer required.
  [Live ruleset](https://github.com/moq-dev/moq/rules/2420853).
- **Runner and watcher evidence.** The repository runner metadata reports
  `nixos-moq-ci` online with `self-hosted` and `moq-queue` labels. The
  [first successful Queue job](https://github.com/moq-dev/moq/actions/runs/41404569287)
  ran there on 2026-11-02, 07:10-07:19 UTC. The host source at
  [kixelated/nixos@a788bea](https://github.com/kixelated/nixos/blob/a788bea07f456d55cd3bbd8bfd60145fdac5256d/hosts/nixos/default.nix#L35)
  enables moq's watcher; its module selects `watch` instead of `serve`,
  retaining the user, state and fixer. Historical activity confirms more
  than configuration intent: the watcher
  [published a fixed head and armed auto-merge](https://github.com/moq-dev/moq/pull/7222#issuecomment-6590060984),
  and [resolved a conflict and armed auto-merge](https://github.com/moq-dev/moq/pull/7501#issuecomment-6597514578)
  at 2026-11-03 07:01 UTC; the latter PR merged at 07:57:56 UTC.
- **Fixer permissions.** A
  [historical workflow re-enqueue refusal](https://github.com/moq-dev/moq/pull/7431#issuecomment-6594966849)
  at 2026-11-03 03:49 UTC must not be mistaken for a current installation
  permission check. Organization installation metadata now lists moq-bot's
  `workflows`, `contents` and `merge_queues` permissions as `write`. The
  deployed watcher mints installation tokens from that App (1039466), so
  workflow-touching fixes and re-enqueues have the scope they need.
- **Organization secrets verified absent.** Paginated
  `GET /orgs/moq-dev/actions/secrets?per_page=100` returned one page,
  `total_count=1`, and one name, neither `APP_CLIENT_ID` nor
  `APP_PRIVATE_KEY`; page 1 had no next link and an explicit page 2 was
  empty with the same total. The organization-copy removal condition is
  satisfied at this observation; no secret values were read. Repository
  copies are also absent (18 repository names, no next page); `bot` and
  `publish` each list both names. Other CI credentials remain tracked by
  [read-only CI](/quest/m0/ci/read-only-secrets.md).
- **Open PRs.** `queue.yml` now also offers a dispatch that reports the
  skipped `Queue` context on an old PR head. A pre-workflow PR can still
  need that report; see its workflow comments. No PR was enqueued and no
  workflow dispatched for this recheck.

Host check, 2026-11-03 about 23:14 UTC (read-only SSH to `nixos`, no secret
values read):

- ✅ Recorded as verified (maintainer, 2026-11-03): the deployed watcher
  matches the configuration above, and no moq `serve` gate remains.
- `quest-land-moq.service` ("moq-dev/moq merge queue watcher") is active.
  Its main process started 2026-11-02 06:48:41 PST, and
  `NeedDaemonReload=no`, so it runs the deployed unit file. The system
  generation is `nixos-system-nixos-26.11.20261014.55bfb4d`, switched
  2026-11-03 09:54 PST.
- It runs `quest land watch --app-id 1039466 --fixer <quest-land-moq-fixer>`
  from quest 0.1.0 under bun, as user `quest-land-moq`, with the App key
  passed by `LoadCredential=github-app-key:...`.
- Retained state is in `/var/lib/quest-land-moq` (mode 0700): `checkout/`
  and the live `moq-dev-moq/`.
- `quest-land-fixer.socket` is listening, and an ephemeral
  `quest-land-fixer@` container was running during the check.
- The only remaining host gate unit is `quest-land-moq-pro.service`, for
  moq.pro. moq-dev/moq has only the watcher. Watchers for quest, rind and
  rust2 are also active.
- Housekeeping, not a separate quest: the state directory still holds
  `codex-repro-*` and `fixer-verified-*` directories left from 2026-10-30,
  which can be deleted.

Completed busy-day measurement: **2026-11-02 08:00 UTC to 2026-11-03 08:00
UTC**, a full 24-hour window following the first successful merge-group build.

| Measure | Completed day | Earlier baseline |
| --- | --- | --- |
| Merged PRs / hour | 139 / 24 = **5.79** | Queue: 5.5 overall; host: about 4-6 |
| Final enqueue to merge, median | **46.6 min** | Queue: 87 min |
| Final enqueue to merge, p90 | **84.2 min** | Queue: 162 min |

Method: paginate REST search for `repo:moq-dev/moq is:pr is:merged base:main
merged:2026-11-02..2026-11-03` (188 results at this recheck, no incomplete
results), then
paginate every result's `GET /repos/moq-dev/moq/issues/{number}/timeline`.
Select merges in the half-open 24-hour window above, and subtract each
PR's last `added_to_merge_queue` before its `merged` event. All 139 have an
enqueue event; use the median and nearest-rank p90. Examples at the window's
ends are [moq-dev/moq#7457](https://github.com/moq-dev/moq/pull/7457) and
[moq-dev/moq#7501](https://github.com/moq-dev/moq/pull/7501).

Final-enqueue latency excludes earlier failed attempts, fixer time before
re-enqueue, and PRs that never merged, as in the queue baseline. No host
latency baseline was recorded.

Performance acceptance (maintainer, 2026-11-03):

- ✅ Keep performance verification open; 5.79 PRs/hour for this day does
  not by itself meet the goal.
- Accept this day as meeting the approximate host range of 4-6 PRs/hour.

Remaining work: reconstruct complete queue occupancy, including removed and
unmerged PRs, and compare the median merges in never-empty hours with the
queue baseline's 6.0. A 24-hour rate is not that statistic. Keep
concurrency 1 while gathering this evidence.

Public API: none. Wire: none.
