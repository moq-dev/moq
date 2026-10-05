# [XS] A transcode fetch stops when nobody wants the group

## Goal

moq-transcode's fetch handler (`rs/moq-transcode/src/rung.rs`, the
`requested_group` path) watches `request.demand()` and drops the request once
it goes unused, so a group every caller abandoned is never encoded.
Test: an abandoned fetch encodes nothing, and a late retry after the last
caller left gets a fresh request rather than racing a stale encode.

## Plan

Found while landing #4708, which moved `group::Request` onto `demand()` and
withdraws an abandoned fetch when its last caller drops. The transcode handler
never watches demand, so a late retry still encodes the group and its
`accept` logs `Duplicate`. Wasted work, not a correctness bug.

Public API: none. Wire: none.

## Related

- [Per-worker transcode epochs](/quest/m0/wildcard/transcode-group-start.md) - also edits `rung.rs`
- [Request linger](/quest/m1/request-linger.md) - its linger also delays the fetch withdrawal this relies on
