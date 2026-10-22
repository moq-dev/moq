# [XS] Mark the publishers' live edge as passthrough

## Goal

The built-in publishers' live-edge reads stay private and carry
`// private: passthrough` instead of a `pending` marker naming this quest:
`track::Subscriber::poll_live` and `latest_sequence` on
`track::{Producer,Consumer,Subscriber}` in Rust, and `wire.ts`
`Broadcast.largest`, `hooks.latestSequence`, and `hooks.bufferedLargest` in JS.
`sh/rs/model-markers.py` passes with no marker naming this quest.

## Plan

Waits for [moq-dev/moq#6138](https://github.com/moq-dev/moq/pull/6138) (public serving cursor) to merge, then applies the markers. Public serving lives on after that only for its own leftover, which this quest does not wait for.

Settled by the maintainer on 2026-10-22 (moq-dev/moq#6278):

- ✅ No public live signal. A reader gets nothing from an idle copy's cache
  until the route's answer shows, `Range::live()` and other relative starts
  resolve against the live feed rather than what an ended feed left (#6278),
  and `largest()` waits out an idle copy (#5924).
- ✅ The publishers' buffered edge stays private as passthrough: it is a
  wire-answer detail (SUBSCRIBE_OK, TRACK_STATUS Largest), and a custom
  publisher answers with `largest()`.
- ✅ Groups cached before live are never served while idle; once live, a
  reader gets what its ranges and max delay hold, as on a cold relay.

Public serving ([moq-dev/moq#6138](https://github.com/moq-dev/moq/pull/6138), still open) re-marks these items as pending on this quest; once it merges, apply the markers.

## Related

- [Public serving](/quest/m1/public-model/serve.md) - moves the publishers' live-edge markers onto this quest
