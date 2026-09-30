# [S] Browser playback survives an estimated group end

## Goal

A `@moq/watch` subscriber never aborts a browser-published track with "group
timestamp is below the live edge" when the publisher's next group starts
inside the previous group's estimated end: the JS consumer enforces
monotonic group starts, like Rust.

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

Decided (maintainer, #4543): group starts are monotonic, and that is the only
hard rule. A group's keyframe may not start below the previous group's start,
and no frame may sit below the start of the group before its own. Frames,
keyframes included, may dip below the previous group's content (B-frames, or
a keyframe overlapping its last frame), so an estimated end is never a hard
edge. A group may start at the same timestamp as the previous one; strictly
increasing starts are not enforced. Group IDs never move backwards. A group
starting before the previous group's start is a restart, a new broadcast.
The Rust `moq-mux` producer and consumer enforce this on `dev` since #4543.

- Reproduce with a mocked clock: cut a group, then resume with a keyframe
  between the last frame and the estimated end.
- Align `js/hang/src/container/consumer.ts` (and the JS producer, if it
  differs) to "group starts monotonic", matching `rs/moq-mux/src/container`.
- Target `dev`, where the #4543 rule lives.

## Related

- [More tests under load](/quest/m1/test-flakes-2/README.md) - other load-only failures, fixed at the cause
