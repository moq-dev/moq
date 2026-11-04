# [S] A Rust relay never replays from group 0 to a lite-03/04 subscriber

## Goal

A Rust relay or publisher serving a lite-03 or lite-04 SUBSCRIBE from a track
copy that is still resolving starts at the request-time live edge, skipping
cached history while delivering groups produced after the request.

## Plan

The maintainer decided on 2026-11-03 to keep the request-time floor and land
after HLS grid export. [moq-dev/moq#7652](https://github.com/moq-dev/moq/pull/7652)
has landed. Its unchanged `a_cursor_records_a_publisher_without_fetch` test,
`just ci origin/main`, and `just test interop --all` pass after merging main
into [moq-dev/moq#7737](https://github.com/moq-dev/moq/pull/7737).

The implementation snapshots the request/answer boundary in Rust and JS.
The local Claude review raised the remaining choices below. Public API and
wire format are unchanged.

## Waiting

- Maintainer: decide the request-time floor for a logical track whose metadata is ready but which has neither a serving nor a parked copy. Options: defer the wire join until the next copy supplies its request snapshot (recommended, so that copy's cached history is skipped), or retain an empty request snapshot and deliver that copy's entire cache. The current subscription resolves from retained metadata before the next route splices, so simply deferring the snapshot lookup does not settle when it is captured.
- Maintainer: settle lite-01/02 parity. Rust deliberately retains the arrival-only floor on those versions, while JS uses the answer snapshot. Options: mirror the Rust exception in JS (recommended, preserving the earlier review's Rust carveout), or extend answer snapshots to both implementations on lite-01/02. After these choices, implement them and rerun checks and local review.
