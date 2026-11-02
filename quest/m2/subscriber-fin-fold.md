# [XS] One predicate for the lite close wait

## Goal

`waits_for_subscriber_fin` / `waitsForSubscriberFin` folds into
`has_track_stream` / `hasTrackStream`, or goes. After
[moq-dev/moq#7296](https://github.com/moq-dev/moq/pull/7296) they always agree.

## Plan

Planned 2026-11-01 (maintainer, follow-up planning after the 2026-11-01 /quest-complete run). Remove whichever has fewer callers, in Rust and JS together.
