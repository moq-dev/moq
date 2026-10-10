# [S] Backport moq-time to release

## Goal

`release` carries `moq-time` and `@moq/time`, published, so consumers pinned
to `release` adopt them without waiting for the breaking migrations on
`dev`.

## Plan

Both are additive. Backport the crate and the package only, not consumer
migrations.

## Required

- [The moq-time crate](/quest/m1/time/crate.md) - the crate to backport
- [@moq/time](/quest/m1/time/js.md) - the package to backport
