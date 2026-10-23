# [XS] Timer cleanups

## Goal

`@moq/net`'s `ConnectionProps.linger` is deleted and the pool always uses its
fixed 2s idle timer. Comments that cite timers which no longer exist are
fixed.

## Plan

Decided with the maintainer on 2026-10-23:

- `ConnectionProps.linger` (`js/net/src/connection/pool.ts`) has no caller
  outside the pool, and the pool keeps the largest value any handle ever
  asked for, even after that handle leaves. Delete the prop instead of
  defining a merge rule.
- `rs/moq-video/src/decode/consumer.rs` cites `TRACK_IDLE_LINGER`, which is
  now `moq_net::model::track::IDLE_LINGER` (crate-private).
- The OBS "Give up after" help text (`cpp/obs/src/moq-settings.cpp`) says the
  broadcast "lingers for viewers across the gap", but the setting is a
  reconnect backoff timeout. Say what it does.

Public API: `@moq/net` breaking (`ConnectionProps.linger` removed). Wire:
none.
