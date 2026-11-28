# [M] Fix moq-net resume reader audit bugs (lock-held retry, unjudged time start, held recovery, expired copy)

## Goal

Four verified bugs left behind by multi-round `moq-net` resume PRs are fixed cleanly in Rust and JS without breaking public API or wire formats:
1. `resume::Reader`'s synchronous retry loop yields instead of spinning indefinitely while holding `Mutex<Reader>`.
2. A raised `Start::Time(t)` over unjudged folded keys below a resume point still backfills to find the group containing `t`, preserving the cache invariant.
3. A frame read whose serving copy is evicted checks `Recover::held()` before `repeats()` skips recovery, so retained copies still serve the frame instead of returning `Err(Evicted)`.
4. Rust keeps late groups landing on an expired replaced copy owed just as JS does, and JS has a regression test for the clean-close callback path.

## Plan

Decided 2026-11-28 (maintainer, `/quest-plan` of the pre-0.22 design-gate audit): ✅ track all four non-breaking `resume::Reader` bugs in `m3` for the post-0.22 patch line (rejected: dropping them).

Caused by [moq-dev/moq#9846](https://github.com/moq-dev/moq/pull/9846), [moq-dev/moq#9718](https://github.com/moq-dev/moq/pull/9718), [moq-dev/moq#9546](https://github.com/moq-dev/moq/pull/9546), and [moq-dev/moq#9750](https://github.com/moq-dev/moq/pull/9750).

1. **Bounded lock-held retry loop (`#9846`):**
   - In `rs/moq-net/src/model/resume.rs:1348-1352` (`:3014-3060`), `loop` retries synchronously under `MutexGuard<Reader>` when `fill_owes_below` returns `Below::Landed` + `sub.disarm()`. Concurrent producer traffic landing stale or headless slots below `first` can spin the executor thread while holding the lock. Bound synchronous retries and yield/re-register cleanly (as Codex round 4 recommended before the agent refused under `afk`).
2. **Raised `Start::Time(t)` over unjudged folded keys (`#9718`):**
   - In `rs/moq-net/src/model/resume.rs:3918-3948` and `js/net/src/resume.ts:1265-1295`, `backfill_gaps` checks `delivered.covers(time..resumed)`, which returns `true` when unjudged keys sit inside `delivered.runs`, skipping backfill and placing the start directly at `t` instead of delivering a containing group (e.g. group 75 spanning `75..85` for `Time(80)`). Treat unjudged keys as uncovered when placing a raised time start so warm fronts match plain readers.
3. **Check `Recover::held()` before `repeats()` skips recovery (`#9546`):**
   - In `rs/moq-net/src/model/frame.rs:1673-1681,1714-1729`, when the front concludes on route B and B's copy is evicted, `routed.recover.repeats(err)` returns `true` and skips `Recover::poll` (including its `held()` lookup of retained copy A). `poll_flight` then finds no handler on B and returns `Err(Evicted)` even though copy A still holds the complete frame (contrary to `#9546`'s `--accept` claim that it only cost one extra upstream fetch). Check `Recover::held` before `repeats()` short-circuits.
4. **Expired replaced copy late groups & JS clean-close test (`#9750`):**
   - Ensure Rust retains late groups landing on an expired replaced copy while owed (matching JS), and add a JS unit test covering `#9750`'s clean-close callback path (`js/net/src/resume.ts:2071-2096`).

Tests: deterministic unit tests for each of the four scenarios in Rust and JS (mirroring `#9718` in both languages).

Public API: none. Wire: none.

## Related

- [Consolidate moq-net track and resume state machines and expand TLA+ coverage](/quest/m3/resume-track-consolidation.md) - addresses the underlying architectural complexity driving this patch chain
- [Model resume skips and failover source closure in TLA+ specs](/quest/m1/verify-resume-failover-sync.md) - brings the TLA+ sync commits up to date
