# [S] Binding surface trim

## Goal

moq-ffi takes route input separately from route observation, uses one watch
verb pattern, and ships no objects without a consumer.

## Plan

Decided 2026-10-22 (API audit):

- `MoqRoute` (`rs/moq-ffi/src/origin.rs`) is both an announce input and an
  observation. Its writable `anonymous` is derived from `hops` on read and
  ignored on input, and `hops` is writable on announce, so a local publisher
  can fabricate a hop chain. Announce takes `MoqAnnounceRoute { cost, epoch }`;
  `MoqRoute` stays observation-only with `anonymous` derived. Fix the doc that
  pairs a route with `MoqOriginProducer::dynamic`, which takes none since
  #5678.
- One watch pattern, `x()` and `x_changed()`: `MoqBandwidthConsumer::peek`/
  `changed` become `rate`/`rate_changed`; `MoqGroupDemand`'s
  `is_used`/`used`/`unused` go with it.
- Remove `MoqGroupRequest::demand` and `MoqGroupDemand`; nothing beyond
  wrapper pass-throughs uses them.
- `MoqBroadcastConsumer::resolve(reference, epoch)` takes two adjacent
  `Option<String>`s; take `MoqRendition`, as the rendition consumers do.
- Document that a dropped `MoqBroadcastFinish` closes the broadcast, which a
  garbage-collected language (Kotlin, Go, Dart) does late.
- moq-c: `moq_bandwidth_set_target` has no consumer beyond the README; keep
  it only if [native rung disable](/quest/m1/simulcast-rung-disable.md) uses
  it. Align `moq_client_defaults` wrapping across languages.

Public API: moq-ffi, moq-c, wrappers breaking. Wire: none.
