# [L] moq-net's last pub items with a single container-reader caller go private

## Goal

`track::Subscriber::next_in_range`, `is_skipped`, and `poll_start` are
crate-private, and so is `Subscriber::with_range` if its last production
caller can leave. Their one consumer is moq-mux's container reader, which
orders groups itself; `track::Ordered` reports a `Gap`, so the reader can
move onto it.

## Plan

What is left of the moq-net sweep, which removed or hid every other item
without a caller.

Findings (2026-11-17, from reading the code when this quest was claimed as
`[S]`): moving the reader onto `Gap` is not an `Ordered` swap, so the quest
is `[L]`.

- `moq-mux`'s `container::Consumer` reads in arrival order
  (`poll_recv_group`) into its own `pending` queue and judges a hole at
  `current` while the groups past it are already buffered: the wall-time wait
  since a later group arrived (`hole_wait`, now
  `Delay::min`), media-timestamp `aged` against the newest buffered content,
  the playout clock `passed`, a full max buffer, and container continuity
  (`poll_continues`, 1 ms contiguity).
- `track::Ordered` holds at most one group back behind a hole and judges it by
  its own reach-versus-newest-frame budget. It never delivers the groups past
  a hole, so the reader could not run the wait, clock, buffer-full, or
  continuity rules above on it. `next_in_range` is what tells the reader
  "nothing will fill this hole" (the `excluded` case) and `poll_start`
  resolves its start; `Gap` covers the first and `Ordered`'s start the
  second, only if the reader drops everything else above.
- The three methods have more callers than the one reader.
  `Subscriber::is_skipped` is also called from `moq-audio` (through
  `container::Consumer::is_skipped`), `moq-stats`, and the ts import tests;
  `Ordered::is_skipped`, already public, would serve those.
  `Subscriber::with_range` has production callers in `moq-mux`
  `container/source.rs`, `moq-json` `window`, and `moq-audio`
  `decode/consumer.rs`, and in `moq-net/benches/origin.rs`.
- The reader's code is being changed by
  [#8984](https://github.com/moq-dev/moq/pull/8984) (hole waiting),
  [#9021](https://github.com/moq-dev/moq/pull/9021), and
  [#8951](https://github.com/moq-dev/moq/pull/8951), so this starts after
  the hole wait lands.

Decided 2026-11-17 (maintainer, `/quest-run m0/m1/m2`):

- ✅ B: keep the quest, rewritten as `[L]` with the hole wait as `Required`
  (rejected: A, delete it and keep the methods public as the reader's
  documented surface; C, shrink it to making `with_range` crate-private
  alone).

Steps:

1. Design the container reader's hole policy on `track::Ordered` first: what
   moves into moq-net (the wait, the clock, the buffer cap, continuity) and
   what stays in the reader. Decide there whether `Ordered` takes the
   wall-time wait that `Delay::min` now means, and how the reader's hole
   tests in `container/consumer.rs` carry over. Added `Ordered` API needs the
   maintainer's say.
2. Move the reader onto it, and the `is_skipped` callers onto
   `Ordered::is_skipped`.
3. Make `next_in_range`, `is_skipped`, and `poll_start` crate-private. Make
   `Subscriber::with_range` crate-private too if the callers listed above can
   leave: `moq-json`, `moq-audio`, and the benches move to
   `Subscription::ranges`; `container/source.rs`'s resubscription needs
   "after group N" with a time start, which a `Range` cannot say, so it
   needs the reader's own cursor to drop that group. Otherwise `with_range`
   stays and this quest records why.
- `origin::Consumer::covering` stays `pub`: moq.pro and moq-net's own
  `benches/origin.rs` call it.

Public API: breaking, removals only unless step 1 adds `Ordered` inputs.
Wire: none.

## Related

- [Browser audio holes](/quest/m2/audio-playout/browser-audio-hole-skip.md) - the browser counterpart of the reader moving onto `Gap`
