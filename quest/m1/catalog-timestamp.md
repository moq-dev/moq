# [S] The catalog producer publishes at the caller's timestamp

## Goal

moq-mux's catalog producer (`catalog::Producer`) can publish an update
stamped with a media timestamp its caller gives, rather than the time it is
written, so a publisher whose catalog changes at a frame (a studio's
composition switching layout) gets hang and MSF output from one producer.
Updates without a timestamp behave as today.

## Plan

[moq-dev/studio#146](https://github.com/moq-dev/studio/pull/146) publishes its
composition through a hand-rolled hang-only `moq_json::snapshot::Producer`,
calling `Timed::new` itself, because the moq-mux producer stamps each emit on
its own clock; it cannot carry MSF beside hang until this lands.

Recommended: the timestamp is on the producer's clock, the same one its
`sync.clock` advertises, and a timestamp older than the last published one is
an error rather than reordered.
