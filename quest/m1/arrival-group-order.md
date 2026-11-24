# [L] An arrival read serves its buffered and re-offered groups in Group Order, in Rust and JS

## Goal

An arrival reader (`recv_group` in Rust, `recvGroup` in JS, and each lite publisher that serves a subscription from one) picks the next group from everything it holds unread in the subscription's `Group Order`: newest first, unless `order` is ascending. That covers two sets the languages order differently today:

- Buffered groups, those that arrived and are still unread with no widening involved, including a replayed cache. Rust walks them in arrival order (`poll_recv_group` in `rs/moq-net/src/model/track.rs`), and JS returns the lowest key first from its key-sorted buffer (`#tryRecvGroup` and `bufferInsert` in `js/net/src/track.ts`).
- Re-offered groups, those a widening newly covers: replacing the ranges with `update`, raising a `withRange` cap over groups held beyond it, or widening a live `max` over groups skipped as stale. Rust already re-offers them in Group Order ahead of its arrival walk (`next_owed`). JS puts them back into its key-sorted buffer (`#reoffer`), so the lowest key goes first.

A reader that keeps up sees no change: with one group held, the order has nothing to pick. A reader holding a backlog or a replay reads it newest first by default, as the draft's publisher transmits it. Delivery stays exactly-once, and an arrival read still never skips a lower group that arrives late, which is why Rust walks arrivals rather than keys. The ordered reader (`ordered()`) keeps key order. The wire and the public API do not change; the arrival read's documented order does.

The lite-07 draft needs no change: its `Group Order` section already says a publisher SHOULD transmit pending Groups in the subscriber's `Group Order`, newest first when descending, and this makes both implementations do so for what they hold.

## Plan

Decided 2026-11-23 by the maintainer, via /quest-run: plan it, as found in [moq-dev/moq#9509](https://github.com/moq-dev/moq/pull/9509) ([thread](https://github.com/moq-dev/moq/pull/9509#discussion_r4675205934)), where the Rust and JS twins of the widened-read test pinned opposite orders. That PR made both sort their widened phase, so they pin only the set of groups.

Decided 2026-11-24 by the maintainer, via /quest-run ([decision](https://github.com/moq-dev/moq/pull/9522#issuecomment-7121668986)): the order for buffered groups (arrived, unread, no widening). ✅ (c) serve them in Group Order in both languages, newest first unless the subscription is ascending (rejected: (a) keep each language's order and document the difference; (b) move JS to Rust's arrival order). It changes Rust's tested arrival order too, so it is folded into this quest, now L and no longer JS-only.

Tests:

- Rust `a_widened_history_read_offers_a_group_it_passed_over` (`rs/moq-net/src/model/track.rs`): drop `read[3..].sort_unstable()` and assert `[2, 1, 3, 4, 0]`, since groups 1 and 2 are buffered when it first reads. Its JS twin `subscription bounds stay reader-local for %s reads` (`js/net/src/track.test.ts`): drop the `widened.sort` and assert the same for the arrival read; the ordered read stays `1, 2, 3, 4`. Add an ascending variant to both, which reads `[1, 2, 3, 0, 4]`.
- Rust `ordered_and_arrival_cursors_are_independent` and JS `the ordered and arrival cursors are independent`: the plain cursor no longer walks arrivals. Make both twins hold `3` and `5` at once and assert `5` first, then `3` first under ascending, whichever arrived first.
- JS `replay and backlogs deliver sparse out-of-order arrivals ascending without sorting`: the default order reads `50` down to `0`, and an ascending variant reads the current expectation. Keep its guard that an arrival never sorts the buffer.
- The tests that pin lowest-first re-offers or arrival-order backlogs move with it. Look in `js/net/src/track.test.ts` (cap raises over held groups, widened ranges), `js/net/src/lite/widened_max_age.test.ts`, and the `recv_group` tests in `rs/moq-net/src/lite/publisher.rs` and `rs/moq-net/src/model/track.rs`, and keep each Rust and JS twin asserting the same order.

Guidance:

- Rust: the arrival walk becomes a pick over what is held unread. Keep the arrival cursor's guarantee that a late lower group is never passed over, and keep the cost of a pick from growing with the backlog: no rescan of the held set per hand-out. One shape: track the arrived-unread keys as a set the walk adds to and `take` removes from, and pick its last (or first, for ascending), as `next_owed` does over `filter.owed`; re-offers and buffered groups may then share one owed set.
- JS: walk the key-sorted buffer from the top below `end` for descending, from `start` for ascending. Keep beyond-cap and out-of-window groups held, as now, and keep `#reoffer`'s groups in the same buffer, since the buffer's order is now the Group Order. The order is `this.#state.update.scalars()?.order`, which defaults to `"descending"`.
- Resume across route copies: both readers pick the lowest group any copy holds next (`Reader::poll_lowest` in `rs/moq-net/src/model/resume.rs`, `#lowest` in `js/net/src/resume.ts`). An arrival read picks across copies in Group Order instead, with a tie still favoring the newer copy; an ordered read stays lowest-first. Re-run `/verify-resume` and update `rs/moq-net/verify/resume/` to the new pick.
- Update the docs that state the arrival order: Rust's `poll_recv_group` and `recv_group`, `poll_recv_next` in `rs/moq-net/src/lite/publisher.rs`, JS's `recvGroup`, the bindings' `recv_group` docs in `moq-ffi`, and `doc/lib/dart/index.md`'s "arrival-order reads". `doc/concept/identity.md`'s "Arrivals are unordered" already says Group Order chooses what goes first.
- Audit callers of an arrival read that drain a replay or backlog and expect oldest first, such as a history range read from `Start::Oldest`, in Rust, JS, `moq-ffi`, and the hang and watch packages. Each moves to `ordered()` or asks `order: ascending`. [Buffered playback subscribes in ascending order](/quest/m3/buffered-ascending.md) already plans that for buffered `<moq-watch>` media.
- Since this changes what a published release's readers return, the PR's Impact section says so as a behavior change, with the migration: ask ascending, or read `ordered()`, to keep oldest first.

No single PR caused this. JS has kept a key-sorted buffer since [moq-dev/moq#2772](https://github.com/moq-dev/moq/pull/2772), and [moq-dev/moq#6717](https://github.com/moq-dev/moq/pull/6717) added the newest-first re-offer in Rust while knowingly leaving JS lowest-first, "since its buffer is ascending". So the PR lists no `## Caused by` culprit.

Public API: none, though the arrival read's documented order changes. Wire: none.

## Related

- [Buffered playback subscribes in ascending order](/quest/m3/buffered-ascending.md) - the JS player whose ascending subscriptions this order serves
