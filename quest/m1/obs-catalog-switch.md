# [S] Catalog switch without a freeze

## Goal

A catalog update during OBS playback keeps the current video and audio
playing until the replacement rendition is subscribed and decoding. A
replacement that fails to resolve or subscribe leaves the old track playing
instead of freezing the source until the next catalog update.

## Plan

`moq_source_subscribe_video` and `moq_source_subscribe_audio` in
`cpp/obs/src/moq-source.cpp` install the new decoder and replace
`conn->video` / `conn->audio` as soon as the catalog arrives, which cancels
the old track before the asynchronous `resolve()` and `subscribe_media()`
calls have succeeded. An unavailable sibling broadcast or a rejected
subscribe freezes playback, and even a successful update costs a gap and a
fresh keyframe wait.

Codex flagged it on [#4281](https://github.com/moq-dev/moq/pull/4281)
([finding](https://github.com/moq-dev/moq/pull/4281#discussion_r4113081104),
[reply](https://github.com/moq-dev/moq/pull/4281#discussion_r4114013943)).
The migration kept the moq-c behavior and said it would go to the maintainer
as a follow-up, but nothing was filed. The maintainer decided in the 09-28
merged-PR audit that it blocks the line.

- Hold the old track and decoder until the replacement yields, then swap.
  Keep "dropping the pending call cancels it" intact, so a second update
  racing the first still leaves exactly one live track.
- An update that leaves the chosen rendition unchanged should not
  resubscribe at all.
- A catalog that removes video or audio still clears it, as today.

Cover the swap, the failed replacement, and the unchanged rendition in the
real-relay source tests (`cpp/obs/test/moq-source-test.cpp`).

## Related

- [Video source replacement](/quest/m1/obs-moq-video/source.md) - later replaces the source's decoder, and should keep this switch behavior
