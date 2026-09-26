# [S] Linear announce replay on subscribe

## Goal

A session that subscribes to announcements receives the initial set in time
linear in the number of announced broadcasts, so a viewer joining a relay
with thousands of routes connects as fast per route as one joining a handful.

## Plan

The moq-lite publisher's initial replay (`AnnounceRun::init` in
`rs/moq-net/src/lite/publisher.rs`) de-duplicates by scanning the pending list
for every route it drains: `initial.retain` on Lite05+, `init.contains` on
the Lite01/02 init. That is quadratic in the replay size.

Measured with `session_join_broadcasts` (2026-09, one relay): lite join takes
199 µs with 1 announced broadcast, 517 µs with 64, and 8.9 ms with 1024,
about 8.7 µs per route at the top. IETF is 7.2 ms at 1024 with no quadratic
scan; its profile is SipHash hashing and `Path` comparison, so check whether
a faster hasher for path-keyed maps pays off on both.

Keep the replay's order and its last-update-wins semantics.
