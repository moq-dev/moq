# [S] lite-07 drops Subscriber Min Age

## Goal

SUBSCRIBE and SUBSCRIBE_UPDATE carry no `Subscriber Min Age`, relays stop
aggregating and forwarding it, and `Delay::min` stays a local setting.

## Plan

Found 2026-11-14 by the [feature review](/quest/m3/release/feature-review.md).
`Delay::min` (`rs/moq-net/src/model/subscription.rs`) is "the `Subscriber Min
Age` on the moq-lite-07 wire", and its own doc says "A publisher may use it,
such as to start a cold join that far back; nothing reads it there yet". It
is still encoded in Rust and JS, aggregated across subscribers in a
`BTreeMap` in `demand.rs`, and forwarded upstream by every relay.

A field frozen with no reader costs every relay an
aggregate and a forward, and a later wire version can add it with its reader.

Decided 2026-11-14 (maintainer, pick on [moq-dev/moq#8764](https://github.com/moq-dev/moq/pull/8764)): ✅ (a) drop the wire field and its aggregate; `Delay::min` stays a local setting (rejected: (b) plan the publisher's cold-join reader).

Public API: none if `Delay::min` stays local. Wire: lite-07-wip SUBSCRIBE and
SUBSCRIBE_UPDATE lose the field.

