# [S] A partial group is never cached as whole

## Goal

A subscription group whose max-delay budget expires before the publisher
finishes it resets with `Old` instead of ending normally, on every hop, in
Rust and JS. A relay that receives it never caches the prefix as the complete
group.

Today a reader that has taken every frame the group holds treats expiry as
losing nothing and ends the group cleanly: `expired_truncates` in
`rs/moq-net/src/model/group.rs` and `#expire` in `js/net/src/group.ts`. A
publisher serving that group then sends a FIN, and the next hop stores the
truncated group as finished.

## Plan

Decided with the maintainer on 2026-10-17: the goal as drafted, from the
follow-up to [#5680](https://github.com/moq-dev/moq/pull/5680), which made an
expiring FETCH reset with `OLD` for the same reason.

The clean end was chosen for a local reader stalled at a group's tail. A
serving publisher cannot tell whether the producer would have written more,
so expiry there is truncation. Decide whether a local reader keeps the clean
end, and keep Rust and JS consistent either way.

Add a regression test that fails without the fix: an upstream group stalls
past the budget after its last frame, and the downstream hop holds no
finished group for it. Wire: no new codes; lite already resets with `Old`.
