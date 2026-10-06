# [XS] A transcode fetch stops when nobody wants the group

## Goal

moq-transcode's fetch handler (`fetch()` in `rs/moq-transcode/src/rung.rs`,
reached from `requested_group`) watches `request.demand()` and drops the
request once it goes unused, so a group every caller abandoned is never
encoded. Test: an abandoned fetch encodes nothing, and a late retry after the
last caller left gets a fresh request rather than racing a stale encode.

## Plan

Found while landing #4708, which moved `group::Request` onto `demand()` and
withdraws an abandoned fetch when its last caller drops. The transcode handler
never watches demand, so a late retry still encodes the group and its
`accept` logs `Duplicate`. Wasted work, not a correctness bug.

Build on #4812, which landed first and made the same `fetch()` refuse a
mid-group start before it fetches the source.

Public API: none. Wire: none.

## Related

- [Transcoders start at group boundaries](/quest/m0/wildcard/transcode-group-start.md) - #4812 changed the same `fetch()`
- [Request linger](/quest/m1/request-linger.md) - its linger also delays the fetch withdrawal this relies on
