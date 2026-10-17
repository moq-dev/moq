# [S] A partial group is never cached as whole

## Goal

A subscription group whose max-delay budget expires before the publisher
finishes it fails with `Old` instead of ending normally, for every reader:
a local reader at the group's tail and a publisher serving it alike, on every
hop, in Rust and JS. A serving publisher resets the stream with `Old`, so a
relay that receives it never caches the prefix as the complete group.

Today a reader that has taken every frame the group holds treats expiry as
losing nothing and ends the group cleanly: `expired_truncates` in
`rs/moq-net/src/model/group.rs` and `#expire` in `js/net/src/group.ts`. The
lite publisher already resets with `Old` when a group expires before its
stream opens or while a write is blocked, but when it is parked at the tail
waiting for the next frame, `poll_next_frame` returns `Ok(None)` and
`GroupServe::poll_serve` (`rs/moq-net/src/lite/publisher.rs`) finishes the
stream. The next hop then stores the truncated group as finished.

## Plan

Decided with the maintainer on 2026-10-17: the goal as drafted, from the
follow-up to [#5680](https://github.com/moq-dev/moq/pull/5680), which made an
expiring FETCH reset with `OLD` for the same reason.

Decided with the maintainer on 2026-10-17: every reader, local or served,
gets `Old` when an unfinished group expires. Rejected: resetting only on the
serving path, which leaves a local reader unable to tell a truncated group
from a whole one, and a separate expired-at-tail state, which adds API for
the same outcome. The clean end was chosen for a local reader stalled at a
group's tail; it goes, along with `expired_truncates`' distinction. Readers
already treat `Old` like a latency skip.

Add regression tests that fail without the fix:

- A Rust model test in `rs/moq-net/src/model/group.rs` and its JS twin for
  `js/net/src/group.ts`: a local reader that has taken every frame of an
  unfinished group receives `Old` when the group expires.
- An upstream group stalls past the budget after its last frame, and the
  downstream hop holds no finished group for it.

Wire: no new codes; lite already resets with `Old`.
