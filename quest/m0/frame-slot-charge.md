# [S] Frame slots are cached for free

## Goal

A group's frame slots are charged against `MOQ_CACHE_CAPACITY`, so a track
producing many small frames per group is billed for what it holds. The fixed
per-group overhead already covers the first few slots; this is the growth past
them, and the capacity a group keeps after eviction.

## Plan

`GroupState::cache` counts frame payload bytes and nothing else. The frames
themselves live in a `VecDeque<Frame>` whose slots are 48 B each, and two
things follow:

- The deque grows geometrically as frames are appended. Every slot past the
  fixed per-group allowance is 48 B the pool never sees.
- `evict` pops from the front and subtracts only `frame.payload.len()`. A
  `VecDeque` never shrinks its capacity, so a group that once held 64 frames
  keeps 3 KB of slots after every payload is gone, charged as zero.

It only bites where many small frames share one group: 50 frames of 200 B
undercharges by about 30%. Video is unaffected, since a group carrying
kilobyte frames is dominated by payload.

The fix is not a bigger constant. `cache` has to start counting the deque's
capacity, which means charging the delta at each of the four `charge.add`
sites in `rs/moq-net/src/model/group.rs` (`write_frame`, the `write_frames`
loop, the partial commit, and `frame_commit`), and keeping `evict` and
`release` consistent with capacity rather than payload.

Decide what `MAX_CACHE_BYTES` compares against before touching any of it.
Today it is a payload limit and the tests read it that way, so folding slots
into `cache` silently changes the per-group ceiling and the point at which a
group starts evicting itself. Either keep a separate payload counter for that
comparison or restate the limit; do not let one field mean both.

`rs/moq-net/tests/group_charge.rs` weighs the process with a counting
allocator and is the place to prove it: add a many-small-frames shape beside
the existing one-frame-per-group case, and a unit test that crosses a deque
growth boundary and asserts the pool charge moved.

### How it was found

CodeRabbit raised it on
[#3523](https://github.com/moq-dev/moq/pull/3523), which fixed the
one-frame-per-group undercount that was OOM-killing relays serving chat. The
finding was correct and deliberately left out of that PR: it is a different
shape, and it changes what `cache` means.

## Related

- [Relay memory](/quest/m2/relay-memory.md) - the per-announcement half of the same question, whose figures also predate the current accounting
