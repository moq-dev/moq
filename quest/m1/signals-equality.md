# [S] Signals notify on every structural change

## Goal

A `Signal` holding a plain object or array notifies whenever a `set` changes
its structure, including a renamed key with an `undefined` value
(`{left: undefined}` to `{right: undefined}`) and an array whose length
changes without enumerable entries (`Array(1)` to `Array(2)`). Class
instances keep identity equality and notification coalescing is unchanged.

## Plan

Decided 2026-10-22 from the repository improvement audit, which reproduced
both missed notifications against `isEqual` in `js/signals/src/index.ts`: it
compares enumerable key counts and values, but not key membership or array
length.

- Fix `isEqual`: compare array `length`, and require each key of one side to
  be an own key of the other.
- Add a fast-check model-based test (fast-check is already used by
  `js/json`'s snapshot properties): commands for subscribe, set (including the
  cases above), microtask flush, unsubscribe, and close, checked against a
  small reference model of which subscribers fire and when. It covers the
  notification and lifetime contract generally, not only this bug.
- Out of scope: an immediately disposed `Signal.watch()` still runs its queued
  initial callback. No consumer depends on either behavior, and changing it is
  an API timing decision; leave it unless the model test shows a consumer
  hazard.
