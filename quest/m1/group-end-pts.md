# [XS] A group ends at its highest PTS

## Goal

A group's end in presentation time is the highest PTS among its frames, not
its newest frame's, in Rust and `@moq/net`, so a group whose decode-order tail
holds B-frames is not judged stale early.

## Plan

Rust overwrites the group's `latest` with each new frame's timestamp
(`rs/moq-net/src/model/group.rs`, `Timeline::Timed` in `stamp` and on
adoption), and so does JS (`js/net/src/group.ts`, `state.latest`). Keep the
maximum instead, and test a group written in decode order with a B-frame
last against a drift budget.

Public API: none. Wire: none.

## Related

- [Rust groups by start](/quest/m1/time-addressing/model.md) - group ends become explicit, but a group without GROUP_END still ends at its highest PTS
