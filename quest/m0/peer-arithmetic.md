# [S] A lite SUBSCRIBE_DROP or SUBSCRIBE_SKIP ending at group u64::MAX accounts for that group

## Goal

A SUBSCRIBE_DROP or SUBSCRIBE_SKIP whose inclusive Group End is `u64::MAX` gives up or skips every group it names, including `u64::MAX`, or is refused.
Today `lite/subscriber.rs` and `lite/subscriber/fills.rs` turn it into `start..u64::MAX` with `saturating_add(1)`, so a reader of group `u64::MAX` is never ended by the drop.

## Plan

Found by the 2026-11-07 invariant audit (lite-3, lite-4).

The rest of the audit's arithmetic findings landed with kio's `Instant` losing its panicking `Duration` operators.
This one is left because the model holds groups, drops, skips, and the tail ledger as exclusive `Range<u64>`, which cannot name group `u64::MAX`, while the model deliberately admits that group (`end_at_maximum_group_is_unbounded` in `model/track.rs`, `a_missing_head_at_the_maximum_sequence_does_not_panic` in `lite/publisher.rs`).

## Waiting

- Maintainer: pick how group `u64::MAX` is accounted. (a) The model refuses creating group `u64::MAX`, so every group fits an exclusive range and a drop ending there names everything that can exist; reverts the two tests above and refuses that sequence from any peer. (b) Inclusive ranges through the drop, skip, and tail accounting; a large refactor for one sequence. (c) Refuse a Group End of `u64::MAX` at decode as a PROTOCOL_VIOLATION, with a draft note; leaves group `u64::MAX` itself admitted but never droppable. Recommend (a): the simplest invariant, removing the special cases at the last sequence across the model, for a value no real publisher reaches.
