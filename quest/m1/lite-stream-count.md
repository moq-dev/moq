# [M] moq-lite-07 counts group streams instead of dropping groups

## Goal

On moq-lite-07, a subscriber knows a subscription has delivered everything
once it has seen as many group streams as the publisher opened, the way
moq-transport's PUBLISH_DONE Stream Count works. SUBSCRIBE_DROP is gone from
lite-07: a group the publisher skipped or never opened is simply not counted,
so nothing has to name it. Published versions (lite-01 to -06) keep decoding
SUBSCRIBE_DROP unchanged.

Where reliable reset is negotiated, the count is exact and the subscriber
waits for nothing else. Where it is not (browsers today), a stream reset
before its header arrived is still invisible, so the track-tail grace stays.

## Plan

- Wire: SUBSCRIBE_END gains `Stream Count`, the number of group streams the
  publisher opened for this subscription. The publisher sends it once every
  group stream below the end has been opened (not finished), like
  PUBLISH_DONE, so the boundary arrives slightly later than today. Remove
  SUBSCRIBE_DROP and its type from lite-07 and reword the Subscribe Stream
  section: the FIN follows once every counted stream has finished or been
  reset. lite-07 is still work-in-progress (`moq-lite-07-wip`), so this changes it in place; update
  `drafts/draft-lcurley-moq-lite.md` and its changelog.
- Rust and JS publishers count the streams they open per subscription and
  send the count; a relay counts its own downstream streams, never forwarding
  the upstream count.
- Subscribers on lite-07 stop waiting once the count is reached, accepting a
  late stream below the end within the grace. On lite-05 and -06, the
  DROP accounting already in moq-net (`rs/moq-net/src/tail.rs`) and `@moq/net`
  (`js/net/src/tail.ts`) stays as it is.
- Tests in both languages: a late stream after SUBSCRIBE_END, a skipped group
  that is never counted, a reset stream, and a count of zero. Add a Rust-JS
  interop case.

This lands before lite-07 is finalized. Published drafts keep SUBSCRIBE_DROP,
which moq-net and `@moq/net` already account. lite-07 replaces it with the
count in both. Rust still does not send SUBSCRIBE_DROP.

## Related

- [Reliable stream reset](/quest/m1/quic/reliable-reset.md) - makes the count exact by keeping a reset stream's header
