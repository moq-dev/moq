# [S] Subscribe ranges end to end

## Goal

One test holds the finished subscribe-ranges work together: a relay with a
sparse cache answers a wide past range plus live, in both orders, over lite-07
and moq-transport, and every sequence in range is delivered or dropped.

## Plan

Every child of the old epic has landed (the last, lite-07 FETCH as a
single-group request, in [moq-dev/moq#8616](https://github.com/moq-dev/moq/pull/8616)),
and the audit checked what is left against the integration suite on 2026-11-14.
Covered in pieces: `subscribe_ranges.rs` (sparse and merged ranges, widening,
time ends, over lite and moq-transport), `range_read.rs` (contiguous history and
live-edge ranges, ascending), `group_order.rs` (both orders through a relay on
every wire, contiguous live groups), `range_fill.rs` (lite-07 recovery), and
`drop_record.rs` (every sequence delivered or dropped through a relay).

Missing is the combination: a relay cache with holes, a past range wider than
what it holds plus the live edge, read in ascending and descending order, on
lite-07 and on moq-transport. Add it beside those tests, reusing their
fixtures, and delete this quest in that PR.

## Related

- [Rust groups by start](/quest/m2/time-addressing/model.md) - the cut reworks `Absolute(group)` ends and sequence-range drops onto times
