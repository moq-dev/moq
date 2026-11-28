# [S] JS arrival reads recompute reach from live state and bound passed steps

## Goal

In `@moq/net`, `#pastBudget` never suppresses fills when an empty or unstamped successor lands below a previously passed group (preserving the cache invariant), and `#passed.steps` stays bounded on long-lived tracks instead of growing without limit while a cached group pins `floor`.

## Plan

Decided 2026-11-28 (maintainer, `/quest-plan` of the pre-0.22 design-gate audit): ✅ fix both bugs before `0.22.0` (overriding the unilateral `afk` `--accept` at score 6/10 on [moq-dev/moq#9746](https://github.com/moq-dev/moq/pull/9746)).

Caused by [moq-dev/moq#9746](https://github.com/moq-dev/moq/pull/9746).

1. **Cache-invariant violation (`js/net/src/track.ts:4899-4927, 3962-3970, 3802-3806, 4076-4094`):**
   - `#pastBudget` replays saved `{key, reach}` entries in `this.#passed.steps` without invalidating them when a successor changes.
   - Because `#reach(key)` returns `own?.end` else the immediate `#successor(key)`'s timestamp and treats an unstamped/empty successor as unbounded (`:3802-3803`), a new empty group landing at key 30 between saved steps at 20 and 40 leaves `#reach(29) === undefined` for a fresh reader, yet `#pastBudget` uses the stale saved `reach` at 40 and permanently suppresses filling group 29.
2. **Unbounded growth (`js/net/src/track.ts:4907-4926`):**
   - While any cached group pins `floor`, `this.#passed.steps` appends an entry per passed group without bound (~180,000 entries/hour at 20 ms audio groups). `#9746` assumed `#9579` (`steady-broadcast-growth`) would handle it, and excluded `#passed.steps` from its own retention test.

Fix: recompute reach from live track state (or invalidate/coalesce `#passed.steps` on successor changes and bound its size by active track state, never one entry per passed group).

Tests: unit tests in `js/net` proving (1) a widened reader still fills group 29 after an empty/unstamped group 30 lands below a previously passed step at 40 (matching a fresh reader), and (2) `this.#passed.steps` stays bounded over thousands of groups even while an old cached group pins `floor`.

Public API: none. Wire: none.

## Related

- [One steady broadcast stops growing the relay and the TS passthrough](/quest/m1/steady-broadcast-growth.md) - related long-lived broadcast memory bounds
