# [XS] A finished JS group allocates no cancel error

## Goal

`@moq/net`'s subscriber reads a group to its FIN without building a
`StreamError` or a transport error, so a 50 group-per-second audio track no
longer pays two stack-captured allocations per group.

## Plan

`js/net/src/lite/subscriber.ts` calls `stream.stop(new StreamError(Cancel))`
after every group, and `js/net/src/stream.ts` converts it again, even though
after a FIN the stop is a no-op.

Decided (2026-10-04): `readFrames` (`js/net/src/lite/group.ts`) reports
whether it reached FIN, and the subscriber stops the stream only when it did
not. Measure the per-group cost before and after with the browser benchmark.

## Closes

- [#4779](https://github.com/moq-dev/moq/issues/4779) - close this issue when the quest finishes

## Related

- [Audio group duration](/quest/m1/audio-group-duration.md) - fewer groups per audio track
