# [XS] Remove effect.cancel

## Goal

`@moq/signals` no longer exports `Effect.cancel`. Its only use was racing a
per-run pending promise, which leaks a reaction per call;
[JS retention](/quest/m0/js-retention.md) deprecates it in favor of
`effect.race` and moves every internal caller off it.

## Plan

A published break to `@moq/signals`, so it targets dev. Delete the getter and
the promise backing it, and fix any caller or doc the deprecation left behind.

## Required

- [JS retention](/quest/m0/js-retention.md) - adds `effect.race` and moves the callers
