# [M] kio::machine writes straight-line poll bodies

## Goal

kio ships `#[kio::machine]` behind its `macros` feature: a `poll_*` body
written as straight-line code with explicit wait points compiles to a named
state enum and a resumable `poll`. It lands in the same PR as its first
users, the straight-line hand-written state machines in `rs/` (about 12,
including `TrackInfoFetch` in `rs/moq-net/src/lite/subscriber.rs`). Loops
and branches that contain a wait stay out of scope.

## Plan

Decided with the maintainer on 2026-10-28, from the
[poll-macro spike](https://github.com/moq-dev/moq/pull/7057): adopt the macro
for straight-line code only, landing with its first users, and add loop
support only once conversions show it is needed.

- **Start from the prototype** in
  [moq-dev/moq@d2fc504](https://github.com/moq-dev/moq/commit/d2fc50458c04d6ab73c2ae4055244b87cdbc111e)
  and [moq-dev/moq@f28d5c6](https://github.com/moq-dev/moq/commit/f28d5c618d82872cd77e8b7a30a182fffa0ed215):
  `#[kio::machine(self.state: Name)]` on an `impl` block, `wait!(Step =>
  poll_expr)` at each wait, `#[every_poll]` guards. Keep kio's rules: no
  `Pin`, named state, `Send` from named field types.
- **Shipping:** `kio-macros` (syn 2, already in the graph) behind kio's
  optional `macros` feature; crates that use it, starting with moq-net,
  enable it, so library-only builds that don't use it pay nothing (decided
  2026-10-28).
- **Errors the landed macro must fix** (decided 2026-10-28):
  - Inside a wait's poll expression, earlier locals are `&mut` borrows, so
    `drop(label)` compiles and silently does nothing. Make that impossible or
    a compile error.
  - A `!Send` local must name the wait and the local that caused it, for
    example with one struct per variant.
  - Not required: keeping a borrow of `self` across a wait gives a misleading
    "missing lifetime" error. Document it as a known limit.
- **Tests:** trybuild UI tests for each error case in the spike's table, and
  the converted machines' existing tests unchanged.
- **Conversions:** every straight-line hand-written state machine in `rs/`;
  list each one, and any left by hand with the reason, in the PR. The
  straight-line `async fn` bodies are
  [their own quest](/quest/m1/poll-first/machine-bodies.md).
- **Docs:** update `rs/AGENTS.md`'s `poll_*` guidance to say when to reach for
  the macro.

Public API: kio gains the `macros` feature and `#[kio::machine]`. Wire: none.
