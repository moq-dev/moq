# [S] Release's audio encoder rolls groups before the cap

## Goal

On `release`, `@moq/publish`'s audio encoder starts a new group before a group
reaches `MAX_GROUP_FRAMES` or `MAX_GROUP_CACHE_BYTES`, so a long
`groupDuration` with short frames never throws `GroupTooLarge`. It ships in
the next release as a patch.

## Plan

Added 2026-11-06 (maintainer, follow-up planning in `/quest-complete`), from
[moq-dev/moq#7279](https://github.com/moq-dev/moq/pull/7279), which shipped
`groupDuration` in `@moq/publish` 0.5.5: a `groupDuration` of tens of
seconds with 2.5 ms Opus frames passes `release`'s per-group frame cap and
throws.

`main` has no group cap ([unbounded groups](/quest/m2/unbounded-groups/README.md)),
so this is a fix on `release` only, a PR into `release` with no `main`
change. Roll the group at the cap the way `js/json`'s window encoder does on
`release` (`MAX_GROUP_FRAMES`, kept below the net cap), and test a long
`groupDuration` with 2.5 ms frames.

Public API: none. Wire: none.
