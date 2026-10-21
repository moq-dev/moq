# [M] Fetch as a range

## Goal

`fetch_group` is gone: every caller reads a past group through a one-range
subscription, which ends once its range is accounted for. That covers the
relay, moq-archive, hls, moq-ffi and every binding wrapper with its `doc/lib`
page, and `moq fetch` through `moq_relay::fetch_group`. A benchmark covers
filling over range count and span as separate axes.

## Plan

Split out of [dynamic range requests](/quest/m1/subscribe-ranges/dynamic.md)
by the maintainer on 2026-10-20
([moq-dev/moq#6123](https://github.com/moq-dev/moq/pull/6123)), so each PR
breaks one API once.

- **Ending a bounded read** (decided 2026-10-20). `recv_group` returns `None`
  once every sequence in a bounded window is delivered, dropped, stale, or
  unfillable. Today it parks past the window's end on purpose, so an update
  can still widen it; a reader that wants more keeps an open range instead.
  A miss surfaces as that `None`, which moq-ffi maps to `NotFound`. The
  `NotFetchable` and `Old` distinctions go, since no caller acts on them.
  Each affected package gets a `MIGRATE.md` entry.
- A one-range subscription for a cached live group missing the frames its
  range starts at needs a head fill, which is why this requires frame bounds.
- Benchmark filling beside `track_range_serving`, which covers serving.

## Required

- [Dynamic range requests](/quest/m1/subscribe-ranges/dynamic.md) - `requested_fill` and subscriptions filling their misses
- [Frame bounds](/quest/m1/subscribe-ranges/frame-bounds.md) - head fills for a range starting mid-group
