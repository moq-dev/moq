# [M] A front keeps an expired copy's late groups owed, and JS matches

## Goal

A logical track a front serves hands out what a plain track would: late groups that land on a
replaced copy after it expired stay owed to a later widened budget. JS
(`js/net/src/resume.ts`) matches Rust's fixes from
[moq-dev/moq#9750](https://github.com/moq-dev/moq/pull/9750).

## Plan

Found by #9750's helpers while killing resume mutants.

- Rust repro (budget 1000ms; routed vs plain, as
  `a_widened_budget_reads_an_expired_copys_groups_past_a_hand_out` in
  `rs/moq-net/src/model/resume.rs` does): write 0 and 5 on A, hold 5, drain 0, switch to B,
  write 6 and drain it. Once A has expired, write late groups 4, 2, 1 on A, then write and hand
  out 7, then widen the budget to 2000ms. Plain gets `[7, 2, 1]`; routed gets `[7]`. Cause:
  `Subscriber::forget_past` keeps a gap only for a group a copy's `lowest_unread` reports, which
  needs the copy's cursor to have polled it, and nothing polls an expired copy. So the hand-out
  of 7 forgets the gaps those late groups fill. #9750 fixed this only for groups the copy had
  peeked before it expired. Likely fix: have `forget_past` keep what such a copy's cache holds
  unread, not just what its cursor took in, with a bound on the walk so a peer can't make it cost
  unbounded work.
- JS parity for #9750's two Rust fixes:
  - `forgetPast` skips a copy through `#feeds`, which applies the budget expiry. Judge the copy
    under any budget, as Rust's `forget_past` now does with `reads(copy, Duration::MAX, ..)`.
  - `#letGo` drops a copy that still has `drains` set without setting `#resumed`, so a group a
    draining copy gave up is never asked of the serving route. Rust's `let_go` now re-arms the walk
    and `poll_first` walks at once. Port both, with `a_group_a_draining_copy_gives_up_is_backfilled`
    and the widened-budget test as JS tests.

Public API: none. Wire: none.
