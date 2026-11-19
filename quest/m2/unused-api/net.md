# [L] moq-net's last pub items with a single container-reader caller go private

## Goal

`track::Subscriber::next_in_range`, `is_skipped`, and `poll_start` are
crate-private, and their js/net mirrors leave its published surface. Their
one consumer is the container reader (moq-mux, and `@moq/hang` in JS), which
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

Hiding is no middle ground: `#[doc(hidden)]` is banned
([moq-dev/moq#8392](https://github.com/moq-dev/moq/pull/8392)), and js/net
keeps `@internal` for deprecated exports.

Step 1's design (2026-11-19, from reading `container/consumer.rs` and
`Ordered`), rule by rule for a hole at the reader's cursor:

- `Ordered` already covers a finished track, keys the ranges exclude
  (`next_in_range`), declared skips (`is_skipped`), and `Delay::max` aged on
  the reach against the newest frame, though only on a track whose moq-net
  groups carry timestamps; on an untimed one it waits until the hole
  resolves, where the reader ages on decoded container timestamps.
- The reader keeps 1 ms continuity (on a `Gap`, it calls the loss only once
  the next group's first frame fails to continue the edge) and the slow-group
  skip, since `Ordered` hands out the next group while the reader still reads
  the current one. A full max buffer past a hole is moot: `Ordered` leaves
  those groups in the transport cache.
- `Ordered` cannot run two rules, since it never hands out the groups past a
  hole: the `Delay::min` wall-time wait since a group past the hole landed,
  and the playout clock reaching where that group starts.

Decided 2026-11-19 (maintainer, `/quest-complete`): ✅ B, `Ordered` owns
the hole policy (rejected: A, two peek/give-up methods for the reader; C, keep
`next_in_range`, `poll_start` and `is_skipped` public). `Ordered` gives up a
hole once `Delay::min` of wall time has passed since a later group landed (an
unstated minimum keeps waiting for `Delay::max`, as today), and takes a
playout-position input that gives up a hole once playback reaches where the
next group starts. Reason: these are player rules any `Ordered` reader that
states a minimum wants, and `Delay::min`'s docs already promise the wait.

Findings while starting B (2026-11-19, from reading its callers):

- `Delay::default()` states `min: Some(0)`, so every reader built from
  `Delay::default().with_max(..)` states a zero minimum. moq-archive's ordered
  store (`producer/track.rs`) does, and under B it would give up a reordered
  group the moment a later one lands, where it waits up to its budget today.
  The bindings' defaults (`moq_subscription_defaults`, libmoq's zeroed
  `moq_delay`) state it too, so a binding user who raises only `max` changes
  as well.
- `Ordered` never hands the reader a group it skips as stale or gives up as
  late, so the reader can no longer report when that backlog landed:
  `container::Consumer::backlog()`, which moq-audio feeds its join estimate
  from, loses most of its signal.
- A local track owes nothing, so `Ordered` steps over its missing keys at
  once and without a gap. The reader then keeps only its 1 ms continuity
  rule there (and calls an undeclared, discontinuous run a gap without
  waiting); its hole tests carry over on a received copy
  (`container::test_util::Link`), not on a local track.
- `Ordered`'s wall-clock deadlines (the new `min` wait and playout position,
  like `Delay::max` today) fire only on a track whose cache pool a driver
  collects, as an origin's does; the reader's own timer goes away.

`@moq/hang`'s container reader calls the js/net mirrors (`start`,
`nextInRange`, `isSkipped`), so the same move applies there in the same PR.

`with_range` stays public (recommended, from the same reading): moq-json,
moq-audio, and moq-mux's `container/source.rs` call it as a local filter on
an established subscription, which `Subscription::ranges`, a wire request,
does not replace.

Steps:

1. Add B's `Ordered` policy: the `Delay::min` hole wait and a playout-position
   input (Rust `Ordered::set_playhead`, js/net `Ordered.setPlayhead`), shaped
   by the decisions under Waiting.
2. Move the reader onto it, and the `is_skipped` callers onto
   `Ordered::is_skipped`.
3. Make `next_in_range`, `is_skipped`, and `poll_start` crate-private.
- `origin::Consumer::covering` stays `pub`: moq.pro and moq-net's own
  `benches/origin.rs` call it.

The reader reads gaps through `Ordered::next_item` once
[ordered-gaps](/quest/m2/outcome-enums/ordered-gaps.md) lands, since
`next_group` then yields groups alone (decided 2026-11-18, maintainer,
/quest-plan); whichever lands second makes the switch.

Public API: breaking; three `Subscriber` methods go crate-private, `Ordered`
gains one input, and `Delay::min` becomes a hole wait for every ordered
reader. Wire: none.

## Waiting

- Maintainer: which ordered readers get B's `Delay::min` hole wait, since
  `Delay::default()` states a zero minimum (see Findings while starting B):
  - 1a (recommended): every reader that states a minimum, zero included, as
    decided; in-tree readers that tolerate reordering (moq-archive, and any
    other `Ordered` reader the audit finds) state `with_min(None)`, and the
    bindings document that a zero minimum now gives a hole up at once.
  - 1b: `Delay::default().min` becomes `None`, so only a stated minimum waits;
    a media reader built from `Delay::default().with_max(x)` then waits `x`
    for a hole instead of zero, and the bindings' defaults change with it.
- Maintainer: how moq-audio keeps learning when a join's backlog landed once
  the reader reads through `Ordered` (see Findings while starting B):
  - 2a (recommended): moq-audio observes every arrival through a second,
    arrival-order subscription, as `@moq/hang`'s reader already does
    (`#observe`), and `container::Consumer::backlog()` is removed (breaking
    in moq-mux, one caller).
  - 2b: `track::Item::Gap` gains when the latest group it gives up had
    landed, which the reader reports as `backlog()`; mirrored in js/net.
  - 2c: `backlog()` reports only what the reader itself discards; moq-audio's
    join estimate can then end before its backlog finished arriving.

## Related

- [Ordered gaps](/quest/m2/outcome-enums/ordered-gaps.md) - splits `Ordered` into `next_group` and the `next_item` this reader uses
- [Browser audio holes](/quest/m2/audio-playout/browser-audio-hole-skip.md) - grading the browser counterpart, which already reads `Gap`s
