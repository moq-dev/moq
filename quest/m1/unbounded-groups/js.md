# [M] JS unbounded groups

## Goal

`@moq/net` has no group frame-count, group byte, or frame size cap. A reader
that falls behind an evicted front gets the same refill-or-`Evicted` behavior
as Rust.

## Plan

See the [epic](/quest/m1/unbounded-groups/README.md) for the decisions.

Decided 2026-10-27 (maintainer): this waits only on
[Front eviction](/quest/m1/unbounded-groups/front-eviction.md), since
removing the caps and adding the JS front trigger need nothing from byte
holes. The chunked frame read and the byte-offset wire mirror moved to
[JS byte holes](/quest/m1/unbounded-groups/js-byte-holes.md).

- Today `js/net/src/group.ts` caps frames and bytes and throws
  `GroupTooLarge`, and JS has no byte pool. Its eviction is whole groups by
  idle time, which never fires on a group that is still being written.
  Removing the caps therefore needs a front trigger first. Shed front runs
  older than the cache window, so a long-lived group holds about one window
  of media. Port the Rust pool if a browser workload needs a byte bound
  instead. Otherwise match the Rust contract.
- Test a single group written continuously for several windows: memory
  stays near one window, and a reader that falls behind gets refill or
  `Evicted`.
- Update `js/json` and its tests, which lean on the removed constants, and
  the compression paths now in `js/net`. The
  [m1 compression migration](https://github.com/moq-dev/moq/pull/7046) deleted `js/flate`.
- If [Rust-backed @moq/net](/quest/m2/rs2ts/README.md) is adopted first,
  Rust's caps and eviction apply and this quest is done.

Public API: removes the JS cap constants. Wire: none.

## Required

- [Front eviction](/quest/m1/unbounded-groups/front-eviction.md) - the Rust contract this mirrors
