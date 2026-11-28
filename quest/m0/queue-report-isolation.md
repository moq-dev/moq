# [S] Remove checks: write from self-hosted queue reporting and drop gh-readonly-queue from cache-write

## Goal

The `main` merge queue requires the native `Queue build / Build` check directly (deleting the self-hosted `report` job and all `checks: write` permissions from `queue.yml` and `queue-build.yml`), and the `cache-write` GitHub Environment no longer admits `gh-readonly-queue/main/*` deployment refs.

## Plan

Decided 2026-11-28 (maintainer, `/quest-plan` of the pre-0.22 design-gate audit): ✅ switch ruleset 2420853 from `Queue` to `Queue build / Build` (deleting `report` and `checks: write`, overriding the earlier `--accept` on [moq-dev/moq#9659](https://github.com/moq-dev/moq/pull/9659) and [moq-dev/moq#9660](https://github.com/moq-dev/moq/pull/9660); rejected: moving `report` to `ubuntu-latest`; keeping self-hosted `report` with `$HOME` isolation), and remove the stale `gh-readonly-queue/main/*` policy from `cache-write` ([moq-dev/moq#9605](https://github.com/moq-dev/moq/pull/9605)).

Caused by [moq-dev/moq#9659](https://github.com/moq-dev/moq/pull/9659), [moq-dev/moq#9660](https://github.com/moq-dev/moq/pull/9660), and [moq-dev/moq#9605](https://github.com/moq-dev/moq/pull/9605).

1. **Eliminate self-hosted `checks: write` in the merge queue (`#9659`, `#9660`):**
   - Today `.github/workflows/queue-build.yml:82-102` runs bare `gh api` with `permissions: checks: write` on the same persistent `[self-hosted, moq-queue]` runner and writable `$HOME` where `build` executes candidate PR code (including auto-enqueued Dependabot PRs via `.github/workflows/dependabot.yml:29-81`), with no retry on 5xx.
   - Switch ruleset 2420853 (and `kixel-ai/dotfiles` `bin/repo-settings` if configured there) from required check `Queue` to `Queue build / Build` (matching `kixel-ai/rind`).
   - Delete the `report` job and `checks: write` permissions from `.github/workflows/queue.yml:30-36`, `.github/workflows/queue-build.yml:82-102`, and `.github/workflows/queue-pr.yml` if no longer needed, and update `CONTRIBUTING.md:39`.
2. **Remove `gh-readonly-queue/main/*` from `cache-write` (`#9605`):**
   - `cache-write` holds `NIX_SIGNING_KEY` and release B2 keys (`infra/README.md:236-237,334-339`), yet `infra/README.md:64` still documents a live `gh-readonly-queue/main/*` deployment branch policy that lets merge-group trees deploy `cache-write`.
   - Delete `gh-readonly-queue/main/*` from the `cache-write` Environment's deployment branch policies via GitHub API and update `infra/README.md:64`.

Verification: confirm via `gh api` that ruleset 2420853 requires `Queue build / Build` and `cache-write`'s deployment branch policies list only `main` and the five release tag patterns, and confirm a queued PR lands cleanly through the merge queue.

Public API: none. Wire: none.
