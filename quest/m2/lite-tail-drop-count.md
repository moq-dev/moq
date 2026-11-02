# [M] moq-lite session tails count drops instead of waiting a grace

## Goal

From lite-05 on, a moq-lite session's tail is complete once every group up to the end has arrived or been reported in SUBSCRIBE_DROP. The 1 s `ledger::GRACE` (`rs/moq-net/src/ledger.rs:19`) then no longer decides it, in Rust and JS.

## Plan

Decided 2026-10-30 (maintainer, follow-up planning): m2. Found by the audit on [moq-dev/moq#7403](https://github.com/moq-dev/moq/pull/7403). The maintainer dislikes grace periods.

- Count per version. lite-01..04 have no drop message, so they keep today's behaviour.
- Account for holes left by lost datagrams.
- Guard against a peer that never accounts for a group: decide with the maintainer whether that ends the tail with an error or keeps waiting. Don't add a new grace.
- The IETF tail keeps its grace until the moq-transport gap-signal extension (planned in [moq-dev/moq#7425](https://github.com/moq-dev/moq/pull/7425)) lands, since moq-transport can't signal gaps.

Public API: none. Wire: none.

## Related

- [Drop set](/quest/m1/drop-set.md) - the per-cursor drop reporting this counts
