# [S] Browser playback survives an estimated group end

## Goal

A `@moq/watch` subscriber never aborts a browser-published track with "group
timestamp is below the live edge" when the publisher's next group starts
inside the previous group's estimated end.

## Plan

Seen once in `js -> js` while two interop matrices ran side by side: the
subscriber logged `skipping covered group: 0 -> 1`, then aborted the video
track with that error, one second into the cell.

Likely cause, not yet reproduced in a unit test: the Legacy producer in
`js/hang/src/container/legacy.ts` closes a group without an explicit end (for
example `cut()` when the encoder pauses for lack of demand) by estimating
`end = last frame + interval` and writing that as the group's end marker. Its
own live edge stays at the last frame, so it accepts a resuming keyframe that
lands before the estimate. The consumer (`js/hang/src/container/consumer.ts`)
takes the live edge from the end marker, so that keyframe's group reads as
below it and the track aborts. The consumer's covered-group skip tolerates the
same overlap the live-edge check rejects.

- Reproduce with a mocked clock: cut a group, then resume with a keyframe
  between the last frame and the estimated end.
- Decide which side is wrong: the producer's estimate reaching past what it
  will refuse, or the consumer treating an estimated end as a hard edge.
  Fix it there and keep the other side's rule consistent with it.
- Check the Rust `moq-mux` producer for the same estimate.

## Related

- [More tests under load](/quest/m1/test-flakes-2.md) - other load-only failures, fixed at the cause
