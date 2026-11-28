# [L] Consolidate moq-net track and resume state machines and expand TLA+ coverage

## Goal

`moq-net`'s `Record`, `Misses`, and `Delivered` bookkeeping is unified into one clear per-key lifecycle state machine across Rust (`rs/moq-net/src/model/`) and JS (`js/net/src/`), `resume::Reader` stops duplicating `track::Subscriber` invariants externally across N cursors with split-lock `Idle`/`disarm` workarounds, and the TLA+ models (`ResumeReader.tla`, `FailoverChain.tla`, `StaleRecover.tla`) cover ordered reads, budgets, range edits, drops, refusals, and multi-route provenance so future changes stop spinning into multi-PR patch chains.

## Plan

Decided 2026-11-28 (maintainer, `/quest-plan` of the pre-0.22 design-gate audit): ✅ plan the architectural consolidation in `m3` after `0.22.0` ships (rejected: continuing one-off patches without addressing root complexity).

Why: the 2026-11-28 audit found an 8-PR, ~80-round patch chain across `track.rs` (40,843 lines), `resume.rs` (17,506 lines), `track.ts` (7,546 lines), and `resume.ts` (3,941 lines) ([moq-dev/moq#9529](https://github.com/moq-dev/moq/pull/9529), [#9564](https://github.com/moq-dev/moq/pull/9564), [#9577](https://github.com/moq-dev/moq/pull/9577), [#9619](https://github.com/moq-dev/moq/pull/9619), [#9718](https://github.com/moq-dev/moq/pull/9718), [#9748](https://github.com/moq-dev/moq/pull/9748), [#9750](https://github.com/moq-dev/moq/pull/9750), [#9846](https://github.com/moq-dev/moq/pull/9846)). Root causes:
1. `resume::Reader` externally multiplexes N `track::Subscriber` cursors, duplicating `forget_past`/`lowest_owed`, `Copy::drains`/`rescan_drains`, `fill_owes_below`/`cached_in`/`disarm`, and nested `unjudged` tracking in both Rust and JS.
2. Fragmented bookkeeping: `Record` holds 6 parallel structures, `Misses` holds 7, `demand.rs` maintains parallel refusal-epoch bookkeeping (`#9495`), and `Idle` gates span separate lock acquisitions.
3. `ResumeReader.tla`, `FailoverChain.tla`, and `StaleRecover.tla` exclude ordered reads, budgets/expiry, range edits, drops, refusals, and caps, leaving the highest-churn paths unverified.

Guidance:
- Builds on [moq-dev/moq#9719](https://github.com/moq-dev/moq/pull/9719) (which deleted `stale_budget`, `Start::Unresolved`, and `withheld`) and coordinates with [recover-group-provenance](/quest/m3/recover-group-provenance.md).
- Follow `REVIEW.md` -> State ("One enum per lifecycle", "One lock per value"): collapse overlapping key-state sets in `Record`/`Misses`/`Delivered` into one explicit per-key state machine, remove split-lock `Idle`/`disarm` retry hacks, and evaluate keeping the Rust lite-07 subscription open on finished tracks (like JS) to delete `demand.rs`'s parallel `FinishedServe`/`refusals` layer.
- Extend `ResumeReader.tla` and `FailoverChain.tla` to cover ordered reads, range edits, budgets, and drops before refactoring `resume::Reader`, and run every config in `/verify-resume` and `/verify-failover`.

Public API: none. Wire: none.

## Required

- [Model source closure before copy failure in FailoverChain.tla](/quest/m3/verify-failover-source-closure.md) - brings the failover TLA+ model and its sync commit up to date

## Related

- [Fix moq-net resume reader audit bugs](/quest/m3/resume-reader-audit-fixes.md) - immediate point fixes for the verified audit bugs
- [Recover group provenance across route switches](/quest/m3/recover-group-provenance.md) - per-group delivery provenance on relay copies
