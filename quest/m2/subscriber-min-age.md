# [S] Decide whether lite-07 carries Subscriber Min Age

## Goal

Before lite-07 freezes, SUBSCRIBE's `Subscriber Min Age` either has a reader
or leaves the wire, with `Delay::min` staying a local setting.

## Plan

Found 2026-11-14 by the [feature review](/quest/m3/release/feature-review.md).
`Delay::min` (`rs/moq-net/src/model/subscription.rs`) is "the `Subscriber Min
Age` on the moq-lite-07 wire", and its own doc says "A publisher may use it,
such as to start a cold join that far back; nothing reads it there yet". It
is still encoded in Rust and JS, aggregated across subscribers in a
`BTreeMap` in `demand.rs`, and forwarded upstream by every relay.

Recommended: (a). A field frozen with no reader costs every relay an
aggregate and a forward, and a later wire version can add it with its reader.

Public API: none if `Delay::min` stays local. Wire: lite-07-wip SUBSCRIBE and
SUBSCRIBE_UPDATE lose the field.

## Waiting

- Maintainer: (a) drop the wire field and its aggregate, keeping `Delay::min` local (recommended); (b) keep it and plan the publisher's cold-join reader before the freeze
