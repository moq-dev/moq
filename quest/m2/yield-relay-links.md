# [S] A plain group yields on a relay link

## Goal

A lite-07 group whose stream is reset to make room pauses (YIELDED) on a
relay-to-relay session too, unless that track carries moq-transport object
records, so a relay short of stream credit stops losing preempted groups for
every viewer behind it.

## Plan

Found 2026-11-14 by the [feature review](/quest/m3/release/feature-review.md).
`GroupServe::yields` (`rs/moq-net/src/lite/publisher.rs`) is
`version.has_yield() && !ctx.ietf`, and `ietf` is per session. moq-relay
offers the `ietf` extension on both its listen and connect sides
(`rs/moq-relay/src/config.rs`), so every relay-to-relay session negotiates it
and no group yields there: a preempted group is reset with TooFarBehind and
lost. Decide per track instead, the way Start Byte refuses only tracks that
carry a MoqTransport Version; narrow the lite draft's YIELDED rule to match,
mirror it in `js/net` if it has the same check, add a test, and run
`/verify-subscribe`.

Public API: none. Wire: lite-07-wip's YIELDED rule narrows from sessions with
the `ietf` extension to tracks that carry moq-transport object records.
