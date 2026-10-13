# [XS] An unset host clock publishes on the 0.17 release line

## Goal

The next 0.17 patch (libmoq 0.6.x, moq-ffi 0.4.x, `@moq/publish`) carries
the unset-clock fix, so a publisher on a host whose wall clock reads before
2020 publishes without a root `clock` instead of aborting the host.

## Plan

Decided 2026-10-13 (maintainer): a crash qualifies for backport
(CONTRIBUTING). Cherry-pick the `main` fix onto `release` as its own PR,
including its tests and the `@moq/publish` mirror. No wire change, so the
release line can take it in a patch.

## Required

- [Unset host clock](/quest/m0/wall-clock-omit.md) - lands the fix on `main` first

## Closes

- [#5407](https://github.com/moq-dev/moq/issues/5407) - moq-mux panics when the system clock is before 2020
