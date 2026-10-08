# [S] Frame slots are cached for free

## Goal

A group's frame slots are charged against the cache pool, so a track producing
many small frames per group is billed for what it holds. The fixed per-group
overhead already covers the first few slots; this is the growth past them, and
the capacity a released group keeps.

## Plan

Re-planned from the closed #3546 against the current accounting in
`rs/moq-net/src/model/group.rs`.

`GroupState::cache` and its `cache::Charge` count frame payload bytes only. The
frames live in a `VecDeque<Frame>`, and `CACHE_OVERHEAD` prices exactly
`FRAME_SLOTS` (4) of its slots, the capacity `VecDeque` rounds the first frame
up to. Two gaps follow:

- The deque grows geometrically past those four slots, and every slot beyond
  them is `size_of::<Frame>()` the pool never sees. It only bites where many
  small frames share one group (chat, telemetry); a group of kilobyte video
  frames is dominated by payload.
- `GroupState::release` (on abort, too-large, or a dropped producer) calls
  `frames.clear()`, which keeps the capacity, then zeroes `cache` and clears
  the charge. A consumer still holding the group pins those slots uncharged.

Decided 2026-10-08, two changes and no more:

- Free the deque's storage in `GroupState::release`, not just `clear()` it.
- Charge the growth past `FRAME_SLOTS` to the pool at each `charge.add` site
  (`write_frame`, the `write_frames` loop, and `GroupState::charge_partial`
  for a streamed frame), keeping `FRAME_SLOTS` as the part `CACHE_OVERHEAD`
  already paid. The fix is not a bigger constant.

`MAX_CACHE_BYTES` stays a payload limit: `GroupState::would_overflow` keeps
reading `cache` as payload bytes, so slot charges go to the pool charge and
never into `cache`. Restating that limit needs a measurement first and is not
this quest.

`rs/moq-net/tests/group_charge.rs` weighs the process with a counting
allocator and is the place to prove it: add a many-small-frames shape beside
the one-frame-per-group case, and a unit test that crosses a deque growth
boundary and asserts the pool charge moved.

Found by CodeRabbit on #3523, which fixed the one-frame-per-group undercount
that was OOM-killing relays serving chat, and deliberately left out of it.

Public API: none. Wire: none.
