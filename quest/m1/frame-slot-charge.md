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
  Dropping the deque's storage on release is likely enough here.

The fix is not a bigger constant. The charge has to follow the deque's
capacity: charge the growth at each `charge.add` site (`write_frame`, the
`write_frames` loop, `create_frame`, and `create_frame_owned`), keeping
`FRAME_SLOTS` as the part `CACHE_OVERHEAD` already paid.

Decide what `MAX_CACHE_BYTES` compares against before touching any of it.
`GroupState::would_overflow` reads `cache` as a payload limit, and the tests
and its doc ("maximum total size of frames") read it that way, so folding
slots into `cache` silently changes the per-group ceiling. Either keep a
separate payload counter for that check or restate the limit; do not let one
field mean both.

`rs/moq-net/tests/group_charge.rs` weighs the process with a counting
allocator and is the place to prove it: add a many-small-frames shape beside
the one-frame-per-group case, and a unit test that crosses a deque growth
boundary and asserts the pool charge moved.

Found by CodeRabbit on #3523, which fixed the one-frame-per-group undercount
that was OOM-killing relays serving chat, and deliberately left out of it.

Public API: none, unless `MAX_CACHE_BYTES` is restated. Wire: none.
