# [M] A cold join reaches back by Subscriber Min Age

## Goal

`Subscriber Min Age` stays on lite-07's SUBSCRIBE and SUBSCRIBE_UPDATE and
has a reader. A subscription that joins live and states one starts that far
behind the live edge: the publisher or relay answering it begins with the
cached group that covers the live edge minus Min Age, so a player's buffer
arrives full instead of filling in real time. A subscription that states
none starts where it does today. `Delay { min, max }` keeps its shape in
moq-net and `@moq/net`.

## Plan

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit):

- ✅ Keep the wire field (rejected: drop it and keep `Delay::min` local).
  This reverses the 2026-11-14 pick on
  [moq-dev/moq#8764](https://github.com/moq-dev/moq/pull/8764), whose PR,
  [moq-dev/moq#8839](https://github.com/moq-dev/moq/pull/8839), was closed
  unmerged on 2026-11-16. The 2026-11-14 reason was that a field frozen with
  no reader costs every relay an aggregate and a forward; the answer is to
  give it the reader, not to remove it.
- ✅ The reader: a cold join reaches back by Min Age, from the answering
  node's cache (rejected: leave it a hint with no defined behaviour).

Facts found by the audit:

- `Delay::min` documents itself as "the `Subscriber Min Age` on the
  moq-lite-07 wire" and says "A publisher may use it, such as to start a
  cold join that far back; nothing reads it there yet"
  (`rs/moq-net/src/model/subscription.rs:57-68`). The draft calls it "a
  hint that asks for nothing".
- It is already stated by players: `rs/moq-audio/src/decode/consumer.rs`,
  `rs/moq-cli/src/play/video.rs`, and `js/watch/src/audio/decoder.ts` set
  `delay.min` to the latency they hold, and both codecs encode it. The
  publisher decodes it into the subscription
  (`rs/moq-net/src/lite/publisher.rs:3084`, `:5285`) and nothing looks.
- Where a subscription starts is decided by one rule today: "A `Start` is a
  floor ... The publisher SHOULD start at the oldest group at or above the
  floor that Expiration has not expired" (draft, SUBSCRIBE). So a reader
  gets everything its Max Age allows or, with the default start, only the
  latest group. Neither is "as much as I am going to buffer": a lossless
  reader (no Max Age) asking from the oldest gets the whole cache.
- The aggregate across readers is the smallest stated value
  (`subscription.rs:553-556`), which a relay forwards upstream.

Recommendations for the PR (the maintainer decided the behaviour, not these
details):

- The rule, in the draft's SUBSCRIBE text beside the floor rule: a
  subscription that follows the live edge and states Min Age starts at the
  newest group that began at or before the live edge minus Min Age, never
  below its `Start` floor and never at an expired group; with no such group
  cached it starts at the oldest one it may. It stays best effort ("from
  its cache"): the node never fetches upstream to satisfy it. The field's
  text loses "a hint that asks for nothing".
- The live edge is the newest frame the node holds. On a timed track the
  distance is in media time, from group starts; on a scale-less track it is
  arrival age on the node's own clock, as expiry already measures it.
- An explicit `Time` start or a resumed position wins: Min Age applies only
  to a join that would otherwise start at the latest group or the oldest
  unexpired one.
- The aggregate a relay forwards: recommend the largest stated value in
  place of the smallest. A relay that subscribes upstream cold fills its
  cache from where the upstream starts, and each reader's own Min Age then
  bounds what the relay hands it, so the smallest starves the reader that
  buffers most. Nothing else reads the aggregate. Unstated still never
  counts. This changes one SHOULD in the draft; settle it in the PR.
- Implement where a subscription's first group is picked in the model, so
  the Rust publisher, the relay, and a moq-transport or pre-07 reader
  (which state none) share it, and mirror it in `js/net`'s lite publisher.
  SUBSCRIBE_UPDATE changing Min Age later moves nothing already started.
- Tests, with time mocked: a track of 500 ms groups and a join stating 2 s
  starts four groups back and plays through to live with no gap; a join
  stating none starts at the latest group as before; a join stating more
  than the cache holds gets what there is; a two-relay chain with a cold
  edge gives the reader its reach-back; a scale-less track reaches back by
  arrival age. Update `doc/concept/moq-lite.md` and the lite-07 changelog
  line for the field. Run `just test interop --all`.

Public API: none; `Delay::min`'s doc changes from "nothing reads it" to the
rule. Wire: lite-07-wip keeps the field; its meaning becomes normative, and
the aggregate rule changes if the recommendation is taken.

## Related

- [Refresh a track by interval](/quest/m2/refresh-interval.md) - the other thing a live join does: it may ask for a new group
