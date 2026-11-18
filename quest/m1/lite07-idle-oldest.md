# [S] A live viewer's fresh lite-07 copy asks only for the live edge

## Goal

A relay or client opening a lite-07 subscription for a viewer that asks only
for the live edge (`Range::live()`) asks its upstream for the live edge, not
for every group from the oldest the max age allows.

## Plan

Seen while testing the [range union fix](https://github.com/moq-dev/moq/pull/9045);
reproduce first. `TrackServe::wire_request` in `rs/moq-net/src/lite/subscriber.rs`
treats a copy whose demand is only open live or oldest ranges as idle and asks
from `producer.idle_newest()`. For a copy that never cached a group that is
`None`, and `Subscription::with_start(None)` turns the ranges into
`Range::span(None, None)`, which since
[moq-dev/moq#8638](https://github.com/moq-dev/moq/pull/8638) starts at
`Anchor::Oldest`. A publisher then saw `Oldest..` for a viewer through a relay
that asked for `Range::live()` alone, on a track whose max age is unlimited:
every group it holds.

Recommended: only an idle copy that cached a group rewrites the start; one
with none keeps the subscriber's own ranges. Test: a `Range::live()` viewer
through a lite-07 relay, asserting the publisher's demand stays `Relative(0)..`.
Check the JS subscriber for the same rewrite.
