# [M] moq-audio's playout estimate times when a packet landed, not when it was read

## Goal

A missing group the container reader waits out no longer reads as jitter in
`moq-audio`'s playout estimate, so a lost packet doesn't raise `moq play`'s
audio delay toward the max delay.

## Plan

Found by [moq-dev/moq#9196](https://github.com/moq-dev/moq/pull/9196), which
moved the container reader onto `Ordered`: a missing group is now given up by
`Delay::max` alone, where the reader used to give it up after the
subscription's `Delay::min`, which `moq-audio` set to its playout estimate.
`decode::Consumer::next` observes each packet at `now()`, when it is read, so
every packet queued behind a hole the reader waited out reads as late by up to
the max delay. The browser doesn't have this: `@moq/watch`'s audio decoder hands
`@moq/hang`'s consumer a second, ordered subscription, so its estimate observes
the track's own frames as they land (`#observe`).

Recommended: observe each packet at `container::Consumer::arrival()`, when its
wire frame landed in this node's cache, which a hole wait doesn't move. That
needs the jitter corpus test (`the_decode_path_conforms_to_the_corpus`) to
drive moq-net's clock, since its replay sets a thread-local read clock that
moq-net's landing times don't follow: run it on tokio's paused clock and
advance it to each arrival. #9196 tried the switch and that test failed only
for this reason (`buildup: packet 75 delays 120 ms, expected 100`).

Public API: none. Wire: none.

## Related

- [Audio playout](/quest/m2/audio-playout/README.md) - the estimator this feeds
