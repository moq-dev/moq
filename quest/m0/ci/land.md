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
  onto the chain (subject is the PR title, body keeps the co-author
  trailers, as CONTRIBUTING describes), and resolves quest-file conflicts
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
- Open question, added 2026-10-29 (maintainer) from
  [moq-dev/moq#7166](https://github.com/moq-dev/moq/pull/7166): the `publish`
  Environment admits `main`, whose ruleset lets repository role 2 bypass
  review, and moq-bot joins that bypass list here. Decide with the bypass list
  whether `main` keeps deploying to `publish`, or publishing narrows to
  `release` and release tags. Ask the maintainer; settings changes are theirs.

Work in this repository:

- The workflow: queue events (landing requested through `quest merge
  --land`, a push dequeuing), one concurrency group, inputs resolved on a
  hosted job, the chain built and tested on `vars.MOQ_QUEUE_RUNNER` with a
  read-only token, and the land job on a hosted runner.
- The main ruleset drops its `merge_queue` rule. It requires a check that PR
  heads never carry, so GitHub's merge button and auto-merge cannot land
  around the gate. The admin role keeps its emergency bypass. Ruleset writes
  are the maintainer's; agents only read.
- `check.yml` and `alert.yml` stop listing `merge_group`, and Check runs
  inside the gate. `.github/actions/rust-cache`, `.github/actionlint.yaml`,
  and `sh/gh/queue.test.ts` follow, and the test asserts the new event policy.
- `dependabot.yml` requests landing through the gate instead of
  `gh pr merge --auto`, which the new ruleset leaves with no path to `main`,
  and keeps its coupled smoke update guard.
- Docs that describe the queue: `CONTRIBUTING.md` (its merge queue section
  and priority rule), `README.md`, `doc/setup/dev.md`, `infra/README.md`,
  `justfile`, `sh/kt/check.sh`, and `test/drill/README.md`.
- `release` keeps its own ruleset and PR checks. It has no queue.
- Measure merged PRs per hour and enqueue-to-merged latency for a busy day
  against the baseline, and record both in the PR.

Public API: none. Wire: none.
