# [XS] One predicate for the lite close wait

## Goal

`waits_for_subscriber_fin` / `waitsForSubscriberFin` folds into
`has_track_stream` / `hasTrackStream`, or goes.

## Plan

Planned 2026-11-01 (maintainer, follow-up planning after the 2026-11-01 /quest-complete run). Remove whichever has fewer callers, in Rust and JS together.

On main the two still differ: `has_track_stream` is true from lite-05, while
`waits_for_subscriber_fin` is true only from lite-07, and the subscriber and
publisher pick `writer.close()` over `close()` by it. They agree on every
version only once
[moq-dev/moq#7296](https://github.com/moq-dev/moq/pull/7296) makes lite-05/06
wait for the subscriber's FIN, so the fold must not change any version's
close behavior. If #7296 is dropped instead, this quest goes too.

## Required

- [A lite-01..06 graceful close delivers the final group](/quest/m1/lite-legacy-close-drain.md) - makes the two predicates agree on every version
