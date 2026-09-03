# [S] GOP overhead verdict

## Goal

A number for what keyframe cadence actually costs, so the choice between a
short GOP and a long GOP with an on-demand keyframe request stops being a
guess.

## Plan

The appeal of a keyframe request is that it buys a much longer GOP: publish at
60 seconds and mint an IDR when a subscriber needs one, instead of paying for
one every 2 seconds so the occasional joiner does not wait. That trade is only
worth a wire message, relay coalescing policy, and publisher-side rate
limiting if the I-frames it saves are expensive.

Measure the bitrate cost at fixed quality across GOP lengths (2s, 10s, 60s) on
the default ladder's rungs, on real content rather than a still image, since a
static scene understates an I-frame's cost and a high-motion one overstates
the saving. Report cost per rung, not just at the top: the 240p rung's
350 kbps is where a periodic IDR hurts proportionally most.

Tune-in is not the counterweight it looks like, and the measurement should not
be framed as if it were. A joining subscriber already receives the live edge
group from its first frame (`floor_of`'s zero budget delivers the live edge;
the latest group is never evicted), so it can decode immediately at any GOP
length, direct sessions included. What a long GOP costs a joiner is catch-up:
decoding up to a GOP of frames to reach live. Price that too, since at 60
seconds it stops being negligible.

Exit criteria: the overhead is written down per rung and per GOP length, with
a verdict on whether a long GOP plus a keyframe request is worth designing.
A verdict of "2 seconds is fine" is a valid, expected outcome and completes
this quest.

## Related

- [Keyframe trigger](/quest/m2/keyframe-trigger.md) - the publisher-side half a
  keyframe request would drive, useful on its own

## Closes

- [#2284](https://github.com/moq-dev/moq/issues/2284) - close this issue when the quest finishes
