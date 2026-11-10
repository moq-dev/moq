# [S] Only the release cut edits MIGRATE.md

## Goal

`just check` fails a PR into `main` that edits any `MIGRATE.md`, unless it is
the release cut writing the version-transition sections. The failure tells the
author to put the migration note in the PR's Impact section instead.
`AGENTS.md` and `CONTRIBUTING.md` keep one line on where the note goes and
drop what the check now enforces.

## Plan

Decided 2026-11-09 (user, /quest-plan of a workflow audit's follow-ups in
[moq-dev/quest#594](https://github.com/moq-dev/quest/pull/594)): ✅ plan the
check in m0 (rejected: m1; leave the rule as prose).

`AGENTS.md` has said since 2026-10-23 that only the release cut edits
`MIGRATE.md`. From 2026-11-03 to 11-09, 53 commits on `main` edited one
anyway, 38 of them `!` PRs, and `MIGRATE.md` was the second most common file
in the queue fixer's conflict notes that week, after `quest/`.

- Decide how the check recognizes the cut, such as its branch name or the
  version bump it carries; `sh/gh/release.sh` and CONTRIBUTING.md's Releases
  section describe the cut.
- The check reads the diff against the base, so it also holds in the merge
  queue, where the PR's own CI is only a signal.
- Also decided 2026-11-09 (user): `!` marks a breaking public API or wire
  change, never a behavior change alone. This PR's CONTRIBUTING.md line says
  so; the migration audit already leaves unmarked behavior breaks to review.

Public API: none. Wire: none.

## Related

- [Migration audit of the breaking cut](/quest/m3/release/migrate-audit.md) - gathers the notes this check keeps out of `MIGRATE.md` until the cut
