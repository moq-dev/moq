# [M] The JS publisher serves the joining FETCH

## Goal

On drafts 14 to 18, the JS publisher answers a single-group joining FETCH
with the head of the group its SUBSCRIBE_OK named as Largest, the way the
Rust publisher does. A Largest Object subscription then gets exactly the
objects it asked for: the head on the fetch stream and the rest on the
subscription, with no gap or overlap. The `tail` interop cells with a
native JS or browser publisher keep passing on every draft, including a
track that finishes right after SUBSCRIBE_OK.

## Plan

Decided with the maintainer on 2026-10-22, after
[moq-dev/moq#6407](https://github.com/moq-dev/moq/pull/6407) made a refused
joining FETCH cost the Rust subscriber only that group's head. Until this
lands, `js/net/src/ietf/publisher.ts` refuses every FETCH, ignores the
filter before draft 22, and serves the latest group whole from object 0,
rounding SUBSCRIBE_OK's Largest to `{G, 0}`.

Three parts, which only work together:

1. **Honor Largest Object before draft 22.** `subscribeRange` serves the
   `nextObject` filter from `edge.next`, as draft 22 already does, and
   SUBSCRIBE_OK reports the true Largest on every draft. Otherwise the
   FETCH's head duplicates the subscription's first objects, and the Rust
   subscriber's "a whole group arrived for one the fill already headed" path
   truncates the group to the head. JS subscribers send `unfiltered` before
   draft 22 and are unaffected. A third-party Largest Object subscriber that
   sends no joining FETCH then loses the head, as the draft says it should.
2. **Serve the head.** Keep a per-session registry of joinable SUBSCRIBEs:
   their track, the advertised Largest, the SUBSCRIBE_OK properties and
   timescale, the ranking, and the unsubscribe signal. `#runFetch` resolves a
   joining FETCH against it: unknown subscription is
   `invalid_joining_request_id`, no Largest or a start past it is
   `invalid_range`, more than one group is `not_supported` (as in Rust and
   the draft-22 fill), and a head evicted from the cache is refused before
   FETCH_OK. Then it writes FETCH_OK (end `{G, k + 1}`, not end of track) and
   a fetch stream named by the FETCH's own request ID, reusing
   `#writeFillGroup`. It is not one of the subscription's counted streams.
   Codec work: `Fetch` keeps the joining fields; a `FetchOk` encoder for
   drafts 14 to 18 (request ID through 16, GROUP_ORDER on 14, empty
   parameters, Track Properties from 15); `FetchFrame` writes draft-14's
   explicit object layout and the status after an empty payload on 14 and 15.
3. **Answer a FETCH after the subscription ends.** In the tail test the track
   is finished before SUBSCRIBE_OK goes out, so PUBLISH_DONE follows at once
   and the FETCH arrives to no subscription. Keep a finished subscription's
   join answerable under the bound
   [Join after a quick finish](/quest/m2/ietf-join-after-finish.md) settles
   for Rust, and mirror it here.

Tests: unit tests per draft for each refusal and for a served head (bytes the
Rust subscriber stitches), and the tail cells
`just test interop --tail --publishers js-native-node,js-native-bun --subscribers rust --versions ietf`
plus the browser publisher on drafts 14 to 18.

Public API: none. Wire: the JS publisher answers a joining FETCH it refused,
and serves a Largest Object subscription from past Largest.

## Required

- [Join after a quick finish](/quest/m2/ietf-join-after-finish.md) - settles the bound part 3 reuses

## Related

- [Headless moq-transport groups](/quest/m2/ietf-headless-group.md) - what a subscriber does with a tail whose head never comes
