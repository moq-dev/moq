# [M] Buffered playback subscribes in ascending order

## Goal

A `<moq-watch>` with a nonzero `buffer` subscribes to its audio and video
tracks with `order: "ascending"`, so the oldest group in its window is sent
first and a voice reply's opening never arrives after its live edge. The audio
decoder's head hold from [moq-dev/moq#6549](https://github.com/moq-dev/moq/pull/6549)
shrinks to one rule: hold the head until it spans the floor, then release it
sorted. Live playback (`buffer` of zero) stays newest-first.

Not here: the protocol's `Group Order` (landed in
[moq-dev/moq#5385](https://github.com/moq-dev/moq/pull/5385)), the buffered
video poster (landed in [moq-dev/moq#6609](https://github.com/moq-dev/moq/pull/6609)), and
ordering for the Python or FFI audio consumers.

## Plan

Motivated by the moq.dev Voice demo: each TTS reply is a fresh track whose
head arrives newest-first, which shifted Chrome's decoder timeline and
overflowed the ring (moq-dev/moq#6523), then clipped each reply's opening
until #6549 held and sorted the head with heuristics.

Decided 2026-10-23 by the maintainer:

- The goal: a subscriber asks for ascending order, so the head-hold heuristics
  from #6549 can go. Voice AI motivates it but is not the only user.
- ✅ All buffered media goes ascending, audio and video. Not chosen: audio
  only; an explicit opt-in independent of buffering.
- ✅ The opt-in is buffered mode itself: a nonzero `buffer` makes every media
  subscription ascending. A buffered player always wants its window from the
  start, and a live one never does, so a second knob adds nothing.
- ✅ Keep a head hold, sized by the floor (the minimum latency): hold frames
  until they span the floor (`delay`), then release sorted. Playback cannot
  start before the floor anyway, so the hold costs no startup latency, and it
  absorbs residual reordering between in-flight streams, which the draft still
  requires a subscriber to tolerate. #6549's "a newer group than the first" and
  max-delay release rules go. Not chosen: delete the hold outright; keep
  #6549's rules unchanged.
- ✅ The hold stays audio-only. Video's startup hold is the poster quest's.
- ✅ Placed in m1, requiring [Finalize moq-lite-07](/quest/m1/lite07-finalize.md):
  lite-06 and earlier serve an ascending request newest-first, so this is a
  no-op on default sessions until lite-07 negotiates by default. `release`
  has no `order` field, so it ships in the next minor, not a 0.6.x patch.
  Not chosen: m2; wiring it now behind the lite-07 gate.

Implementation:

- `subscribeMedia` (`js/watch/src/media.ts`) takes the order. Audio and video
  pass `"ascending"` when `sync.out.buffered` is true, and re-subscribe or
  update when buffering toggles. Check how `Subscription.order` updates over
  SUBSCRIBE_UPDATE on lite-07, and that moq-transport fixes it at SUBSCRIBE.
- A relay requests descending upstream unless every subscription it serves is
  ascending. A voice reply track has one subscriber, so it goes ascending end
  to end. Say in the watch docs that a mixed audience gets newest-first
  upstream.
- The draft says an ascending subscriber SHOULD raise `Subscriber Max Age`.
  Buffered mode already subscribes with `delay + buffer`; confirm that covers
  it.
- Replace `Order`'s release rules in `js/watch/src/audio/order.ts` with the
  floor span, keeping `admit()` and the epoch reset ordering from #6549. Keep
  the decoder tests for a reversed head (now released at the floor) and the
  fresh-encoder case.
- Interplay with [Container reads drop backfill](/quest/m1/read-backfill.md):
  once the JS consumer drops groups below its cursor, an ascending head
  arrives above it, so nothing valid is dropped. Whichever lands second
  re-runs the other's tests.
- Update `doc/concept/use-case/ai.md` and the `buffer` docs in
  `js/watch/src/element.ts` and `js/watch/src/sync.ts` in the same PR.
- Verify against a voice agent on a lite-07 relay, such as a Pipecat pipeline
  publishing each TTS reply as a fresh track with one Opus frame per group:
  every reply decodes from its first frame, plain and barge-in, with no
  overflow.

### Runtime mode switch (2026-10-24 pre-cut audit)

Changing `buffer` at runtime does not switch the audio ring's mode:
`js/watch/src/audio/decoder.ts:293` captures `sync.out.buffered` once when it
builds the worklet, and `#order()` (`:679`) peeks it once per attempt, so
going from live to buffered after playback started leaves `Backpressure`
and the worklet `Init` live (future-dated TTS audio is shed), and the
reverse leaves a live player in play-through. Key the ring and `Order` on
`sync.out.buffered`: rebuild the ring, or post a mode message plus a reset.

## Required

- [Finalize moq-lite-07](/quest/m1/lite07-finalize.md) - an ascending request is served newest-first until lite-07 negotiates by default

## Related

- [Container reads drop backfill](/quest/m1/read-backfill.md) - the JS consumer stops delivering groups below its cursor
- [Subscribe ranges](/quest/m1/subscribe-ranges/README.md) - owns the wire's `Group Order` and the ordered cursor's floor
