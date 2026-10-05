# [XS] A finished JS stream allocates no cancel error

## Goal

`@moq/net` reads a group to its FIN without building a `StreamError` or a
transport error, so a 50 group-per-second audio track no longer pays two
stack-captured allocations per group.

## Plan

`js/net/src/lite/subscriber.ts` calls `stream.stop(new StreamError(Cancel))`
after every group, and `js/net/src/stream.ts` converts it again through
`withCode`, even though after a FIN the stop is a no-op. Seven other call
sites build the same cancel error.

Decided (2026-10-04): fix it in the Reader for every caller. The Reader drops
its stream reader once the stream is done, and `stop(code)` builds the error
only while a stream is still open. No benchmark for an XS change; the Bun
microbenchmarks in `js/net/bench` may show it.

Test: stopping a Reader after FIN allocates nothing and sends nothing;
stopping one mid-stream still cancels with the code.

## Closes

- [#4779](https://github.com/moq-dev/moq/issues/4779) - close this issue when the quest finishes

## Related

- [Audio group duration](/quest/m1/audio-group-duration.md) - fewer groups per audio track
