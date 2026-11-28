# [S] A front keeps late groups that land on an expired copy owed

## Goal

A logical track a front serves in Rust (`rs/moq-net/src/model/resume.rs`) hands out what a plain
track would: late groups that land on a replaced copy after it expired stay owed to a later
widened budget. JS (`js/net/src/resume.ts`) already does.

## Plan

Found by [moq-dev/moq#9750](https://github.com/moq-dev/moq/pull/9750)'s helpers while killing
resume mutants. #9750 kept an expired copy's unread groups owed only for groups the copy's cursor
had already peeked before it expired.

Repro (budget 1000ms, routed vs plain, shaped like
`a_widened_budget_reads_an_expired_copys_groups_past_a_hand_out`): write 0 and 5 on A, hold 5,
drain 0, switch to B, write 6 and drain it. Once A has expired, write late groups 4, 2, 1 on A,
then write and hand out 7, then widen the budget to 2000ms. Plain gets `[7, 2, 1]`; routed gets
`[7]`. JS gets `[7, 2, 1]` for both, since its `lowestBuffered` scans the copy's cache.

Cause: `Subscriber::forget_past` keeps a gap only for a group that a copy's
`track::Subscriber::lowest_unread` reports. That needs the copy's cursor to have polled the group,
and nothing polls an expired copy, so the hand-out of 7 forgets the gaps those late groups fill.
Recommended fix: have `forget_past` ask what such a copy's cache holds unread, as JS does, with the
walk bounded by what the copy caches so a peer can't make it unbounded.

Public API: none. Wire: none.
