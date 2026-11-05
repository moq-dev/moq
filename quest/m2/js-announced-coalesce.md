# [S] JS announced coalescing

## Goal

`@moq/net`'s `Announced` holds at most one pending event per path, as Rust's
announce consumer does, so an application that reads slowly while routes churn
holds memory proportional to distinct paths, not to the number of changes.

## Plan

Decided 2026-11-01 (maintainer, announcement backpressure interview): today
`js/net/src/announced.ts` appends every start, end, restart, and update to a
`Signal<Event[]>`, and `#runAnnounced` in `js/net/src/origin.ts` pushes a
diff on each wake. Match Rust's `OriginConsumerState`: a pending change for a
path overwrites the earlier one, and a retraction of a start the reader never
saw cancels it. The JS lite publisher already coalesces this way
(`js/net/src/lite/publisher.ts`).

Tests: churn on one path while the reader is idle leaves one pending event;
a start then end before a read leaves nothing; a restart after a read is
still delivered.

## Related

- [Announce credit](/quest/m2/announce-credit.md) - bounds how many paths there can be
