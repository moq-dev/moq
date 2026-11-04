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

Decided 2026-11-04 (maintainer, /quest-unblock): subscription front with
metadata but no current copy: ✅ defer the wire join until the next copy
supplies its snapshot. Not chosen: joining at the edge the metadata implies.

Decided 2026-11-04 (maintainer, /quest-unblock): lite-01/02 parity:
✅ mirror Rust's arrival-only exception in JS. Not chosen: leaving JS different.
