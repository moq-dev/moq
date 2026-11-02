# [S] JS ordered reads wait only on owed groups

## Goal

`@moq/net`'s ordered reader on a subscribed track waits at a hole while the feed may still deliver that group, bounded by its max delay, as Rust's does since [moq-dev/moq#7403](https://github.com/moq-dev/moq/pull/7403). It no longer skips the gap, and local tracks still skip.

## Plan

Decided 2026-10-30 (maintainer, follow-up planning): m1, for parity with Rust before the cut. Port #7403's rule rather than invent a JS one:

- Only subscription-fed copies owe groups.
- An unlimited budget waits until the hole resolves, with no grace period.
- A finite budget gives up on the same media-time and wall-clock rule.

Port #7403's Rust tests, using JS's manual clock.

Public API: none. Wire: none.

## Related

- [Ordered reads wait only on owed groups](https://github.com/moq-dev/moq/pull/7403) - its PR, #7403, is the Rust rule this ports
