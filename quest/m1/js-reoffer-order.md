# [M] A JS arrival read offers what a widening takes back in Rust's Group Order

## Goal

`js/net`'s arrival reader (`Subscriber.recvGroup` and the lite publisher's `#tryRecvGroup` in `js/net/src/track.ts`) offers the cached groups a widening newly covers in the same order as Rust: newest first, unless the subscription's `order` is `"ascending"`. A widening is replacing the ranges with `update`, raising a `withRange` cap over groups held beyond it, or widening a live `max` over groups skipped as stale. Rust gets this order from `next_owed` (`rs/moq-net/src/model/track.rs`), and it is the draft's default `Group Order` ("newest first when descending (the default), oldest first when ascending"). JS walks its key-sorted buffer and returns the lowest key first.

The draft makes `Group Order` a transmission SHOULD, so this is parity, not a wire fix: the JS publisher serves a widened subscription the way a Rust one does. The ordered reader (`ordered()`) keeps key order, and the wire and public API do not change.

## Plan

Decided 2026-11-23 by the maintainer, via /quest-run: plan it, as found in [moq-dev/moq#9509](https://github.com/moq-dev/moq/pull/9509) ([thread](https://github.com/moq-dev/moq/pull/9509#discussion_r4675205934)). That PR made both twins sort their widened phase, so they pin only the set of groups. This quest puts an order assertion back in both languages:

- Rust `a_widened_history_read_offers_a_group_it_passed_over` (`rs/moq-net/src/model/track.rs`): drop `read[3..].sort_unstable()` and assert `[1, 2, 3, 4, 0]`.
- JS `subscription bounds stay reader-local for %s reads` (`js/net/src/track.test.ts`): drop the `widened.sort` and assert `[4n, 0n]` for the arrival read (still `[4n]` for the ordered one).
- Add an ascending variant to both, which reads `0` before `4`.

Guidance:

- The order belongs to the re-offer, not to the buffer as a whole. Rust re-offers what is owed ahead of its arrival walk, in the subscription's order, then walks arrivals. JS's `#reoffer` puts groups back into the key-sorted buffer, where they lose their identity as re-offers. Mirror Rust's split: keep what a widening owes apart from the buffer and pick its newest (or oldest, for ascending) first. The order is the subscriber's `this.#state.update.scalars()?.order`, which defaults to `"descending"`.
- The JS tests that pin lowest-first re-offers move with it. Look in `js/net/src/track.test.ts` (cap raises over held groups, widened ranges) and `js/net/src/lite/widened_max_age.test.ts`, and compare each with its Rust twin, such as the `"held back, now in range, newest first"` and `"newest first, never 0, 3, or 4 again"` assertions in `rs/moq-net/src/model/track.rs`.
- Update the `recvGroup` doc comment so it says what Rust's `poll_recv_group` says about widenings.
- Check whether a reader resuming across route copies (`#tryResumed`, `js/net/src/resume.ts`) re-offers in the same order as Rust's `rs/moq-net/src/model/resume.rs`. Align it here if they differ.

No single PR caused this. JS has kept a key-sorted buffer since [moq-dev/moq#2772](https://github.com/moq-dev/moq/pull/2772), and [moq-dev/moq#6717](https://github.com/moq-dev/moq/pull/6717) added the newest-first re-offer in Rust while knowingly leaving JS lowest-first, "since its buffer is ascending". So the PR lists no `## Caused by` culprit.

Public API: none. Wire: none.

## Waiting

- Maintainer: decide the order for plain buffered groups, those that arrived and are still unread with no widening involved. The two languages differ here too, and it is a separate choice from the widening order above. Rust's arrival walk returns them in arrival order: `ordered_and_arrival_cursors_are_independent` reads `5` before `3` when `5` arrived first. JS returns the lowest key first: its twin `the ordered and arrival cursors are independent` reads `3` before `5`, and `replay and backlogs deliver sparse out-of-order arrivals ascending without sorting` pins key order on replay. The brief said "its buffered groups ... newest first", which is neither language's current order for these. Options: (a) keep each language's order and document the difference; (b) move JS to Rust's arrival order, which means replacing the key-sorted buffer, in a quest of its own (L); (c) serve plain buffered groups in `Group Order` in both languages, also in a quest of its own. Recommended: (b). It finishes the parity this quest starts, keeps Rust's documented and tested order, and changes only JS. (c) would change Rust's tested order as well. This quest goes ahead either way: it changes only what a widening takes back.

## Related

- [Buffered playback subscribes in ascending order](/quest/m3/buffered-ascending.md) - the JS player whose ascending subscriptions this order serves
