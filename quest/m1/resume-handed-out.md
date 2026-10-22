# [S] Rust resume keeps a backup for a group read from a single route

## Goal

A Rust `resume::Reader` keeps a replaced copy that holds any group it handed
out and is still reading, including a group first read while only one copy
existed. Today such a group is never recorded, so the copy that later holds it
is dropped and the read waits out the reader's `max_delay` before ending with
`Error::Old`, even though the missing frames were available.

## Plan

Found by an external audit on 2026-10-22, confirmed on main. `hand_out`
(`rs/moq-net/src/model/resume.rs`) pushes onto `out` only when
`copies.len() > 1`, a guard from
[#6141](https://github.com/moq-dev/moq/pull/6141). Reproduction: read the
first frame of group 0 from A; swap to B, which holds all of group 0; B's
route fails, keeping its finished group; swap to C, which lacks group 0, and
read group 1; A's partial group 0 fails. B was dropped, so group 0 stalls.

- Decided 2026-10-22 (maintainer): record every handed-out group, dropping the
  guard, as [#6136](https://github.com/moq-dev/moq/pull/6136) does in JS. The
  existing sweep keeps the list at the live group count.
- Decided 2026-10-22 (maintainer): add a passthrough benchmark in
  `rs/moq-net/benches` that reads groups through a resumed track with a
  single copy, so the per-group record's cost shows up.
- Regression test on a mocked clock for the reproduction above.

Lands after [#6224](https://github.com/moq-dev/moq/pull/6224), which edits the
same reader, so it does not widen a PR under review.

Public API: behavior only. Wire: none.

## Required

- [Rust resume holds a declared end](/quest/m1/resume-rust-end.md) - lands first; it edits the same reader

## Related

- [Resumed close](/quest/m1/resume-closed.md) - carries the JS half of this fix
