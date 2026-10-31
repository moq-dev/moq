# [M] JS unbounded groups

## Goal

`@moq/net` has no group frame-count, group byte, or frame size cap. A group
holds a 32 MB tail and sheds its front past it, as Rust does, and a reader
that falls behind the shed front gets the same refresh-or-`Evicted` behavior
as Rust.

## Plan

See the [epic](/quest/m1/unbounded-groups/README.md) for the decisions.

Decided 2026-10-27 (maintainer): this waited only on Rust front eviction
([moq-dev/moq#6993](https://github.com/moq-dev/moq/pull/6993)), since
removing the caps and adding the JS front trigger need nothing from byte
holes. The chunked frame read and the byte-offset wire mirror moved to
[JS byte holes](/quest/m1/unbounded-groups/js-byte-holes.md).

- Today `js/net/src/group.ts` caps frames and bytes and throws
  `GroupTooLarge`, and JS has no byte pool. Its eviction is whole groups by
  idle time, which never fires on a group that is still being written.
  Removing the caps therefore needs a front trigger first: shed the front
  past a 32 MB tail, matching Rust (decided 2026-10-28, maintainer,
  replacing a front trigger by the cache window's age). Port the Rust pool
  only if a browser workload needs a smaller byte bound. Otherwise match
  the Rust contract.
- Test a single group written continuously past several times the tail:
  memory stays near 32 MB, and a reader that falls behind refreshes or gets
  `Evicted`.
- Update `js/json` and its tests, which lean on the removed constants, and
  the compression paths now in `js/net`. The
  [m1 compression migration](https://github.com/moq-dev/moq/pull/7046) deleted `js/flate`.
- If [Rust-backed @moq/net](/quest/m2/rs2ts/README.md) is adopted first,
  Rust's caps and eviction apply and this quest is done.

Public API: removes the JS cap constants. Wire: none.
