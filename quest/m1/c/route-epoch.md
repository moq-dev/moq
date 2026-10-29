# [S] moq-c routes carry their epoch

## Goal

A received `moq_route` exposes its epoch, as moq-ffi's `MoqRoute` does, so a C
subscriber can pin `moq_origin_request` or `moq_origin_follow` to the
instance it saw announced.

## Plan

Decided 2026-10-29 (maintainer), a follow-up of
[moq-dev/moq#7193](https://github.com/moq-dev/moq/pull/7193), which gave the
C resolutions an optional epoch. Today a C caller can pin only an epoch it got
elsewhere: received routes carry none. In m1, since moq-c ships at the cut
with parity with moq-ffi.

[Announce route](/quest/m1/c/announce-route.md) adds minting and announcing
an epoch. This quest covers the receiving side. Expose the epoch as UUIDv7
text, as the resolutions take it, and test that a pinned request built from a
received route refuses another instance.

Public API: additive in the C ABI, or breaking if `moq_route`'s layout
changes; record which. Wire: none.

## Related

- [Announce route](/quest/m1/c/announce-route.md) - the publishing side's epoch
