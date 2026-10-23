# [XS] Every queued PR passes its own group

## Goal

The main merge queue merges a PR only when that PR's own group passes `Check`.
A failed entry leaves the queue instead of riding along behind a later green
group.

## Plan

Decided 2026-10-22 (maintainer): switch ruleset 2420853's merge queue from
`grouping_strategy: HEADGREEN` to `ALLGREEN`. Rejected: checking every group's
whole diff since main, which keeps HEADGREEN but widens each run's selection.

Why: main broke twice on 2026-10-22, and in both cases the queue had already
caught it. The group for [moq-dev/moq#6318](https://github.com/moq-dev/moq/pull/6318)
failed `quest check` on a link to a quest that
[moq-dev/moq#6142](https://github.com/moq-dev/moq/pull/6142) deleted, and the
group for [moq-dev/moq#6222](https://github.com/moq-dev/moq/pull/6222) failed
typechecking a call that [moq-dev/moq#6230](https://github.com/moq-dev/moq/pull/6230)
added. Under HEADGREEN a later group passed, so the failed entries merged anyway.
A later group cannot cover an earlier failure, because `sh/changed.sh` diffs each
group against `merge_group.base_sha` (the previous group), so each run selects
only its own PR's packages.

- This is a repository setting, so the maintainer applies it. Record the change
  with `gh api repos/moq-dev/moq/rulesets/2420853` before and after.
- Keep `max_entries_to_build` and `max_entries_to_merge` unless the change
  shows a throughput problem. Compare queue wait over the first busy day
  against 2026-10-22's run.
- `CONTRIBUTING.md` already describes what each group checks, and that
  stays true.

Public API: none. Wire: none.

## Related

- [CI workflow audit](/quest/m2/audit-ci.md) - owns per-job cost; this quest only changes the gate
