# [S] JS LOC producer writes the duration marker

## Goal

`@moq/loc`'s `Producer` ends each video group with the empty frame that closes
its last frame's duration, as `moq-mux`'s LOC producer does after
[#4450](https://github.com/moq-dev/moq/pull/4450). Readers from
`@moq/loc` 0.2.3 and `moq-mux` 0.10.0 on skip it.

## Plan

The JS producer (`js/loc/src/index.ts`) has no frame kind, cut, or finish: it
closes a group on the next keyframe and on `close()`. Write the marker at both
points, and extend the producer tests to assert it and that the JS consumer
skips it. Low urgency: only a test and a `js/hang` re-export use the producer
today; `js/publish` does not.

Public API: none. Wire: the same marker Rust writes.

