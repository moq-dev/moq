# [S] Remove checks: write from self-hosted queue reporting and drop gh-readonly-queue from cache-write

## Goal

The `main` merge queue requires the native `Queue build / Build` check directly (with `.github/workflows/queue-pr.yml` posting a skipped `Queue build / Build` on PR heads for queue admission, and deleting the self-hosted `report` job and all `checks: write` permissions from `queue.yml` and `queue-build.yml`), and the `cache-write` GitHub Environment no longer admits `gh-readonly-queue/main/*` deployment refs.

## Plan

Decided 2026-11-28 (maintainer, `/quest-plan` of the pre-0.22 design-gate audit): ✅ switch ruleset 2420853 from `Queue` to `Queue build / Build` (deleting `report` and `checks: write`, overriding the earlier `--accept` on [moq-dev/moq#9659](https://github.com/moq-dev/moq/pull/9659) and [moq-dev/moq#9660](https://github.com/moq-dev/moq/pull/9660); rejected: moving `report` to `ubuntu-latest`; keeping self-hosted `report` with `$HOME` isolation), and remove the stale `gh-readonly-queue/main/*` policy from `cache-write` ([moq-dev/moq#9605](https://github.com/moq-dev/moq/pull/9605)).

Caused by [moq-dev/moq#9659](https://github.com/moq-dev/moq/pull/9659), [moq-dev/moq#9660](https://github.com/moq-dev/moq/pull/9660), and [moq-dev/moq#9605](https://github.com/moq-dev/moq/pull/9605).

Today `.github/workflows/queue-build.yml:82-102` runs bare `gh api` with `permissions: checks: write` on the same persistent `[self-hosted, moq-queue]` runner and writable `$HOME` where `build` executes candidate PR code (including auto-enqueued Dependabot PRs via `.github/workflows/dependabot.yml:29-81`), with no retry on 5xx. Meanwhile `cache-write` holds `NIX_SIGNING_KEY` and release B2 keys (`infra/README.md:236-237,334-339`), yet `infra/README.md:64` still documents a live `gh-readonly-queue/main/*` deployment branch policy.

Stage the queue migration in order so PR admission never stalls (GitHub requires a PR's required checks to pass or skip on the PR head before admitting it to the queue, and `Queue build / Build` only runs on `merge_group` unless staged on PR heads too):

1. **Stage 1 PR (before switching the ruleset):** update `.github/workflows/queue-pr.yml` (both its PR trigger and its existing `workflow_dispatch` backfill path) so PR heads receive a skipped `Queue build / Build` check alongside the existing skipped `Queue` check while `Queue` still gates the queue, and update `sh/gh/queue.test.ts` for the dual-check transition (preserving the check that `workflow_dispatch` can post only to an open PR head).
2. **Backfill existing open PR heads:** once Stage 1 lands on `main`, dispatch `queue-pr.yml` for existing open PR heads whose last check ran before Stage 1 and verify every open PR head reports `Queue build / Build`, so no open PR head is blocked from entering the queue when the ruleset switches.
3. **Maintainer admin steps (listed under `## Waiting` below):** once Stage 1 is on `main` and open PR heads are backfilled, switch ruleset 2420853 (and `kixel-ai/dotfiles` `bin/repo-settings` if configured there) from `Queue` to `Queue build / Build`, and delete `gh-readonly-queue/main/*` from `cache-write`'s deployment branch policies.
4. **Stage 2 PR (after the ruleset switch):** delete the `report` job and all `checks: write` permissions from `.github/workflows/queue.yml:30-36` and `.github/workflows/queue-build.yml:82-102`, drop the old `Queue` check from `.github/workflows/queue-pr.yml` (keeping only the skipped `Queue build / Build` check on PR heads and dispatch), update `sh/gh/queue.test.ts` accordingly (still preserving the open-PR-head dispatch check), and update `CONTRIBUTING.md:39` and `infra/README.md:64`.

Verification: confirm via `gh api` that ruleset 2420853 requires `Queue build / Build` and `cache-write`'s deployment branch policies list only `main` and the five release tag patterns, and confirm Stage 2 lands cleanly through the merge queue.

Public API: none. Wire: none.

## Waiting

- Maintainer (action): once Stage 1 has landed on `main` and existing open PR heads have been backfilled and verified to carry `Queue build / Build` via `queue-pr.yml` dispatch, switch ruleset 2420853 (and `kixel-ai/dotfiles` `bin/repo-settings`) from required check `Queue` to `Queue build / Build`. An agent token lacks repo-admin rights to edit rulesets. Done when `gh api repos/moq-dev/moq/rulesets/2420853 --jq '.rules[] | select(.type == "required_status_checks") | .parameters.required_status_checks[].context'` prints `Queue build / Build` and not `Queue`.
- Maintainer (action): delete the `gh-readonly-queue/main/*` deployment branch policy from the `cache-write` GitHub Environment. An agent token lacks repo-admin rights to edit environment deployment policies. Done when `gh api repos/moq-dev/moq/environments/cache-write/deployment-branch-policies --jq '.branch_policies[].name'` does not list `gh-readonly-queue/main/*`.
