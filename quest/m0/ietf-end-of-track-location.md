# [XS] A capped IETF stream never claims END_OF_GROUP

## Goal

A subgroup stream cut short by the subscription's end Location does not set
the END_OF_GROUP bit in its header, in `rs/moq-net` and `js/net`. Today it
does, so a subscriber infers the group ended at the cap when objects past it
exist.

## Plan

Triaged from Fastly's follow-up on #5020 (runs of 2026-10-07 on `0f310a5`,
d18 and d21, cells `status-end-of-track` and `publish-done`).

Decided 2026-10-08: shrunk to this bug. Moving END_OF_TRACK onto the
upstream's Location (5/5 on group 5's stream rather than 6/0 on a new
stream) is conformance polish with no lost data; deferred to an m1
follow-up.

Rust: the subscription's group header in `rs/moq-net/src/ietf/publisher.rs`
takes `has_end` from `GroupFlags::default()`, which is true. Clear it when
`GroupSlice::until` cuts the group short. JS: the matching header in
`js/net/src/ietf/publisher.ts` hard-codes `hasEnd: true`.

Test: a subscription whose end Location falls inside a group gets that
group's stream with a header without END_OF_GROUP, in both languages.

Public API: none. Wire: none (a capped stream's header bit changes, not its
format).
