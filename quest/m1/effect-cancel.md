# [XS] Remove effect.cancel

## Goal

`@moq/signals` no longer exports `Effect.cancel`. Its only use was racing a
per-run pending promise, which leaks a reaction per call. It is already
marked `@internal`, and every internal caller uses `effect.race` instead.

## Plan

A published break to `@moq/signals`, so it targets dev. Delete the getter and
the promise backing it, and fix any caller or doc the deprecation left behind.
