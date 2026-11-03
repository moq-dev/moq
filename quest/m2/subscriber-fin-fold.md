# [XS] One predicate for the lite close wait

## Goal

`waits_for_subscriber_fin` / `waitsForSubscriberFin` folds into
`has_track_stream` / `hasTrackStream`, or goes.

## Plan

Planned 2026-11-01 (maintainer, follow-up planning after the 2026-11-01 /quest-complete run). Remove whichever has fewer callers, in Rust and JS together.

[moq-dev/moq#7296](https://github.com/moq-dev/moq/pull/7296) made lite-05/06
wait for the subscriber's FIN, so on main both predicates now return
`has_track_stream()` / `hasTrackStream(version)`: the wait is only an alias.
`has_track_stream` has far more callers, so fold the alias into it, keep its
doc comment's reasoning at the subscriber and publisher call sites, and leave
every version's close behavior unchanged.
