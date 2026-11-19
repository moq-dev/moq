# [S] A key-order read holding a copy's cursor wakes when the owed group lands

## Goal

A key-order read on a front's logical track that leaves a copy's cursor below a group
outside its window, because the window still owes a group under it
(`Reader::poll_lowest`, the `owes_below` break in `rs/moq-net/src/model/resume.rs`),
wakes when that owed group lands on the copy.

## Plan

Found by [moq-dev/moq#9144](https://github.com/moq-dev/moq/pull/9144) while killing the
`owes_below` mutants: in `a_key_order_read_waits_below_a_group_outside_its_window_only_while_one_is_owed`,
a read polled with a real waiter after the break stays unwoken when group 0 lands on copy
b. The copy's peek returned the reserved group, so nothing registered a wake for a lower
key landing, and the read's idle shortcut then answers `Pending` until something else
fires (another copy's group, a route change, a deadline). With the replaced copy dead it
stalls until the budget runs out.

Reproduce it with that setup and a `Wake` waiter, then register the copy's arrivals
(`track.poll_arrived`, as the drained arm does) before the break, and assert the wake.

Public API: none. Wire: none.
