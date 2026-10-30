# [M] Land main through quest land

## Goal

`main` changes only through a batched gate. `quest land` takes every PR whose
landing was requested and builds them, in enqueue order, into a chain of
squash commits on top of `main`. It runs `just ci` once on the chain's tip on
the self-hosted queue runner, then fast-forwards `main` to the longest
passing prefix of the chain. GitHub's merge queue is gone. Merged PRs per
hour on a busy day rise measurably over the baseline below, while `main`
only ever holds a tree the gate tested.

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
- One self-hosted runner stays the gate. This replaces the planned second
  queue runner (`queue-concurrency.md`, deleted).
- The engine is `quest land` in kixelated/quest, shared with kixelated/quest,
  moq.pro, and kixelated/rust2. This repository adds a thin workflow, as
  moq.pro's `gate.yml` and `queue.yml` do today.
- The gate lands by fast-forward. For each PR it writes the squash commit
  onto the chain, with the engine's default message, and resolves quest-file conflicts
  with quest's merge driver. It force-pushes each PR's branch, with a lease
  on the head it tested, to that PR's commit, then fast-forwards `main` to
  the tip in one push. GitHub marks a PR merged once its head is on the base,
  so the PRs show as merged, `main` stays linear, and the landed tree is
  exactly the tested one. A conflict outside `quest/` ejects that PR, and so
  does a fork branch that blocks maintainer edits, whose head the engine
  cannot move: it is ejected as `no_maintainer_edits`
  ([kixelated/quest#229](https://github.com/kixelated/quest/pull/229)).
- Batches take every queued PR, with no size constant. On failure the gate
  tests the first half. A passing prefix lands at once and the rest
  continues. A PR that fails alone is ejected with a comment linking the
  run. There are no retries.
- The rule that quest, test, and CI PRs jump the queue is dropped. Order is
  enqueue order.
- The push credential is moq-bot, added to the main ruleset's bypass list.
  Its key (`APP_ID`/`APP_CLIENT_ID`, `APP_PRIVATE_KEY`) moves from repository
  secrets into an Environment limited to `main`, `release`, and release tags.
  That way a workflow on a same-repo PR branch can no longer read a key that
  can push to `main`. The release workflows (some run on pushes to
  `release`), `dependabot.yml`, and `smoke-toolchain.yml` that use it
  reference the Environment. Only the hosted land job reads it, never the job
  that runs PR code.
- Decided 2026-10-29 (maintainer), answering the question raised in
  [moq-dev/moq#7166](https://github.com/moq-dev/moq/pull/7166): the moq-bot
  key moves to its own `bot` Environment, for `main`, `release`, and
  `moq-ffi-v*`. Dependabot, the Smoke toolchain update, and the moq-bot-only
  publishers name it. `publish` keeps the key too, for release-rs, which also needs the
  crates.io token. Narrowing `publish` to release refs waits on
  [its own quest](/quest/m0/ci/publish-release-only.md), since `workflow_run`
  publishers run as `main`.
- Decided 2026-10-30 (maintainer): the land job holds no key. It trades its
  GitHub ID token at `token.moq.dev` ([moq-dev/moq#7275](https://github.com/moq-dev/moq/pull/7275)),
  which issues a moq-bot token scoped to the calling repository only for its
  `land.yml` on `main`, so opting another moq-dev repository into the gate
  needs no secret. No other job in `land.yml` may request an ID token or call
  another workflow.
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

Draft source in [moq-dev/moq#7024](https://github.com/moq-dev/moq/pull/7024)
prepares the three phases, trusted Quest installation, the read-only worker,
head-pinned Dependabot requests, bot-key Environment declarations, and the
event policy and setup tests. No workflow was deployed, queue entry changed,
ruleset written, or credential moved. `release` keeps its own PR checks.

Remaining before completion:

- **Maintainer cutover.** On 2026-10-27 the main ruleset still requires
  `Check`, contains `merge_queue`, and admits only the administrator bypass.
  Add the bot bypass and replace those two requirements with `quest land`,
  pinned to an app that never reports it on PR heads. Coordinate source
  installation with that ruleset change: removing `merge_group` cannot
  pass the old queue, and changing protection first stops that queue.
- Decided 2026-10-29 (maintainer): CONTRIBUTING no longer prescribes the
  squash commit body; the engine's default message stands.
- **Forks.** The land job's moq-bot token is scoped to this repository, and
  whether an installation token can push to an editable fork's branch is
  unverified (moq-dev/quest's land-adopt quest tests it live). Until then a
  fork PR may be ejected as `push_failed`, which CONTRIBUTING's fallback
  covers. Forks were 2 of the last 200 merged PRs on 2026-10-30.
- **Live evidence.** Exercise the gate and its ejections after the
  maintainer's cutover, including an editable fork, and measure a busy
  day's merged PRs/hour and enqueue-to-merged latency against the baseline.
  Local event-policy tests do not establish those outcomes.

Public API: none. Wire: none.
