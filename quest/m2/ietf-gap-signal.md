# [M] moq-transport extension that signals dropped groups

## Goal

A moq-transport peer that negotiates a SUBSCRIBE_DROP-style extension tells its subscriber which groups it dropped. An ordered reader on that subscription stops waiting at the hole, as it already does on moq-lite with SUBSCRIBE_DROP. Peers without the extension behave as today.

## Plan

Decided 2026-10-30 (maintainer, from [moq-dev/moq#7403](https://github.com/moq-dev/moq/pull/7403)):

- Unsignalled holes are a moq-transport protocol bug. Until a peer signals them, an ordered reader with an unlimited budget blocks at an owed hole on a moq-transport SUBSCRIBE. That's accepted for now and documented on `Ordered`/`max_delay`.
- No FETCH for holes: fetching a hole to learn whether it will ever fill races the live subscription, so it is rejected.
- No inference from later groups, and no grace periods.
- Build it only when an IETF client wants the fix: define the extension (a setup parameter plus a drop message modelled on moq-lite's SUBSCRIBE_DROP), implement it in Rust and JS, and propose it to the IETF draft. Until then this quest waits on that demand.

Public API: none. Wire: a negotiated moq-transport extension.

## Related

- [Drop set](/quest/m1/drop-set.md) - moq-lite's per-cursor drop reporting this mirrors
