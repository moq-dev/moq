# [S] Check runs on ready main PRs as a signal

## Goal

The Check workflow runs on every non-draft PR against `main` again, and on
each push to one. It runs the same changed-lane selection the land gate runs
(`just ci` against the PR's base) and cancels a superseded run. It is a signal
only: it is not a required check, and it never blocks the land gate. The gate
stays the only thing that decides a landing. Whoever picks the PR with
`/quest-merge` sees the result, and the land fixer reads it as context when
the PR's batch fails.

## Plan

Decided 2026-11-01 (maintainer, /quest-plan): "I think for moq we should
re-enable Check being run on PRs. The problem was that Github was throttling
us and there was a lot of churn. But I think it's still a useful signal [...]
(open source repos do this because it's free, closed source do not)."

- Check's role:
  - ✅ A signal only
  - Gate the fixer: a queued PR sits out batches until Check finishes
  - A required check in the ruleset
- Which PRs trigger it, to keep churn and throttling down:
  - ✅ Ready-for-review PRs and their pushes; drafts don't run
  - Every PR push
  - Only queued PRs
- How much it runs:
  - ✅ The gate's selection: `just ci` against the PR's base
  - Fast lanes only
  - Everything (`CI_BASE: --all`, as release PRs do)

Today `check.yml`'s job has `if: github.base_ref == 'release'`, so main PRs
report Check as skipped
([moq-dev/moq#7024](https://github.com/moq-dev/moq/pull/7024)). Add `main`
for non-draft PRs (`github.event.pull_request.draft == false`, plus the
`ready_for_review` trigger), with `CI_BASE` set to the PR's base for main and
`--all` kept for release. Keep `cancel-in-progress`. The repository's
`MBX_R2_*` pair is already read-only
([Read-only pull-request CI](/quest/m0/ci/read-only-secrets.md)), so a PR
run restores the cache without write credentials. Keep Check out of the
`main` ruleset's required checks. moq.pro stays without PR CI: it's private,
where Actions minutes cost money.

## Related

- [Read-only pull-request CI](/quest/m0/ci/read-only-secrets.md) - what keeps a PR run's secrets read-only
- [Land main through quest land](/quest/m0/ci/land.md) - the gate that stays authoritative
