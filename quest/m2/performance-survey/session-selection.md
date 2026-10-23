# [S] Selective session operations avoid full registry scans

## Goal

Exact-session listing and revalidation avoid unnecessary work across other
sessions while maintaining the existing filter semantics.

## Plan

[Registry::list and revalidate](/rs/moq-relay/src/session.rs) iterate every
entry while holding the registry mutex (lines 114-132), even though
`Filter::id` is exact and the table is keyed by that ID. Views/IDs are cloned
and matching leases woken while the lock is held. Operators and concurrent
connection registration/drop are the consumers.

Sweep sessions × concurrent queries/revalidations, comparing exact-ID hits
and misses, selective multi-field filters and all-session requests. Measure
CPU/query, allocation volume, lock hold/wait, response latency, and concurrent
registration/drop latency. Verify an ID plus other filters still means all
fields must match, and only matching sessions are nudged.

A direct keyed lookup is the minimal candidate. Broad listing necessarily
scales with output; do not add secondary indexes for it without independent
measurement. Existing relay auth-refresh benches do not measure this
administrative selection path.

Follow the [survey's benchmark contract](/quest/m2/performance-survey/README.md):
measure the baseline before optimizing, retain paired evidence, verify equivalent
work, schedule coverage nightly, and accept a measured no-win. Expected public
API and wire impact: none; report the actual impact in the implementation PR.
