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

Measured with `session_join_broadcasts` (2026-09-25, one relay, Apple M4):
lite-06 join takes 128 µs with 1 announced broadcast, 317 µs with 64, and
5.0 ms with 1024, about 4.9 µs per route at the top; lite-07-wip matches.
IETF is 95 µs, 311 µs, and 4.05 ms, with no quadratic scan, so the scan is
the ~1 ms gap at 1024. An earlier Linux profile of the IETF path was SipHash
hashing and `Path` comparison, so check whether a faster hasher for
path-keyed maps pays off on both.

Keep the replay's order and its last-update-wins semantics.
