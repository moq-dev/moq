# [M] Land main through quest land

## Goal

`main` changes only through a batched gate: `quest land serve`, a daemon on
the maintainer's host, shared with moq-dev/quest. It takes the oldest PR whose
landing was requested plus up to floor(log₂ n) more that merge cleanly, in
enqueue order, and runs `just ci` once on the chain's tip in an isolated
container. A passing batch lands one squash commit per PR, each exactly the
tree the gate tested; a failing one is ejected together and handed to a
Codex fixer. GitHub's merge queue and the Land workflow are gone. Merged PRs
per hour on a busy day rise measurably over the baseline below.

## Plan

Baseline, measured 2026-10-26 over 2026-10-23 to 10-26 (414 queue merges,
76.7 h):

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

Decided 2026-10-26 (maintainer):

- GitHub's merge queue cannot do this. Its "merge limits do not combine
  `merge_group` builds": every entry still runs its own CI. HEADGREEN with
  more concurrent builds needs more runners. Mergify (free for open source,
  with batching and bisection) was rejected: it merges through the API, so
  it can neither guarantee the tested tree nor resolve quest-file conflicts.
- The engine is `quest land` in moq-dev/quest, shared with moq-dev/quest,
  moq.pro, and moq-dev/rust2.
- The rule that quest, test, and CI PRs jump the queue is dropped. Order is
  enqueue order.
- Decided 2026-10-29 (maintainer), answering the question raised in
  [moq-dev/moq#7166](https://github.com/moq-dev/moq/pull/7166): the moq-bot
  key moves to its own `bot` Environment, for `main`, `release`, and
  `moq-ffi-v*`. Dependabot, the Smoke toolchain update, and the moq-bot-only
  publishers name it. `publish` keeps the key too, for release-rs, which also needs the
  crates.io token. Narrowing `publish` to release refs waits on
  [its own quest](/quest/m0/ci/publish-release-only.md), since `workflow_run`
  publishers run as `main`.
- Open, found 2026-10-29: `APP_CLIENT_ID` and `APP_PRIVATE_KEY` are moq-dev
  organization secrets visible to this repository, so any workflow on a
  pushed branch can read the moq-bot key, not only jobs in `bot`. Once
  moq-bot bypasses `main`, that branch could push to `main`. Before cutover,
  the maintainer deletes the organization secrets and this repository's own
  copies; `bot` and `publish` already hold the key under the same names.
- The gate tests each PR on the latest `main`, not the PR's own base. On
  2026-10-29 [moq-dev/moq#7162](https://github.com/moq-dev/moq/pull/7162)
  passed its own CI but broke in the queue on API renames that landed after
  its last base merge; that is the case the gate must catch before `main`.

Decided 2026-10-30 (maintainer, from moq-dev/quest's
[host gate epic](https://github.com/moq-dev/quest/blob/main/quest/m0/host-land/README.md)
and [moq-dev/quest#297](https://github.com/moq-dev/quest/pull/297)). This
replaces the 2026-10-26 design of a three-phase `land.yml` on the self-hosted
queue runner, and the 2026-10-30 `token.moq.dev` decision:

- **Host daemon.** `quest land serve` runs on the maintainer's NixOS host as
  its own user, which loads moq-bot's App key through a systemd credential.
  No GitHub Actions job lands anything: `land.yml` goes, and `dependabot.yml`
  stops calling `quest land resolve`, requesting landing with
  `quest merge --land` like any other PR. The `token.moq.dev` land endpoint
  ([moq-dev/moq#7275](https://github.com/moq-dev/moq/pull/7275)) and the
  rule that no other `land.yml` job may request an ID token go with them.
- **Check.** `just ci` runs once per batch on the chain's tip, in an
  ephemeral nspawn container with no credentials, as moq's runners already
  are on that host. The daemon fetches with a read-only token only once
  private-repository support lands in moq-dev/quest; moq is public.
- **Batches** are the oldest queued PR plus up to floor(log₂ n) more that
  merge cleanly with the quest merge driver. There is no bisection: a failing
  batch is ejected together with the log's tail in each comment, and a Codex
  fixer makes only mechanical fixes, requeues healthy PRs, and hands back the
  rest. This supersedes the 2026-10-26 "every queued PR, test the first half"
  rule.
- **Landing.** For each PR in order, the daemon merges the current `main`
  into the PR's branch with the quest merge driver and pushes it, then
  squash-merges the PR through GitHub's API with its head pinned. The squash
  is the tested tree, quest-file conflicts are resolved by the driver rather
  than GitHub, and the PR shows as merged. A fork branch the daemon cannot
  push to lands by pushing its squash to `main` and closing the PR with a
  link, so forks are no longer ejected as `no_maintainer_edits`. This
  supersedes the 2026-10-26 force-push-and-fast-forward landing; Mergify's
  rejection still stands, since it cannot resolve quest files.
- Decision prompts (maintainer, 2026-10-30):
  - Landing: push `main` and close fork PRs / API squash merge per PR /
    ✅ merge `main` into each PR with the driver, then API squash merge, and
    push `main` and close the PR for a fork the daemon cannot push to
  - moq's migration: ✅ rewrite this quest before the cutover / a separate
    migration quest after it / wait for the daemon to run live first
- **Pin.** moq keeps its current moq-dev/quest flake input, whose
  three-phase `quest land` this repository's draft still targets, until the
  cutover bumps it to a release with `quest land serve`.

Draft source in [moq-dev/moq#7024](https://github.com/moq-dev/moq/pull/7024)
prepared the three phases, trusted Quest installation, the read-only worker,
head-pinned Dependabot requests, bot-key Environment declarations, and the
event policy and setup tests. The host daemon supersedes its workflows; its
Dependabot and Environment pieces may still apply. No workflow was deployed,
queue entry changed, ruleset written, or credential moved. `release` keeps
its own PR checks.

Remaining before completion:

- **Host.** moq-dev/quest's
  [gate host](https://github.com/moq-dev/quest/blob/main/quest/m0/host-land/host.md)
  quest runs the daemon for moq-dev/quest first; add moq-dev/moq to it.
- **Maintainer cutover.** On 2026-10-27 the main ruleset still requires
  `Check`, contains `merge_queue`, and admits only the administrator bypass.
  Add the bot bypass and replace those two requirements with `quest land`,
  pinned to an app that never reports it on PR heads. Coordinate the daemon's
  start with that ruleset change: removing `merge_group` cannot pass the old
  queue, and changing protection first stops that queue.
- Decided 2026-10-29 (maintainer): CONTRIBUTING no longer prescribes the
  squash commit body; the engine's default message stands.
- **Live evidence.** Exercise the gate and its ejections after the
  maintainer's cutover, including a fork that blocks maintainer edits, and
  measure a busy day's merged PRs/hour and enqueue-to-merged latency against
  the baseline. Local tests do not establish those outcomes.

Public API: none. Wire: none.
