# [M] Fetch as a range

## Goal

`fetch_group` is gone: every caller reads a past group through a one-range
subscription, which ends once its range is accounted for. That covers the
relay, moq-archive, hls, moq-ffi and every binding wrapper with its `doc/lib`
page, and `moq fetch` through `moq_relay::fetch_group`. A benchmark covers
filling over range count and span as separate axes.

## Plan

Split out of dynamic range requests, which replaced `requested_group` with
`requested_fill`, by the maintainer on 2026-10-20
([moq-dev/moq#6123](https://github.com/moq-dev/moq/pull/6123)), so each PR
breaks one API once.

- **Ending a bounded read** (decided 2026-10-20). `recv_group` returns `None`
  once every sequence in a bounded window is delivered, dropped, stale, or
  unfillable. Today it parks past the window's end on purpose, so an update
  can still widen it; a reader that wants more keeps an open range instead.
  `Ok(None)` before any group arrives is a miss, which the moq-ffi fetch
  wrapper reports as `NotFound`; after one, it is the range's end. A one-range
  read surfaces no `NotFetchable` or `Old` result, and a peer's reset with
  either names the group dropped. Every fetch boundary already treats
  `NotFetchable` as not found (relay HTTP 404, hls `is_cache_miss`, moq-ffi
  `map_fetch_error`, moq-c `NOT_FOUND`); a fetched `Old` becomes a miss too,
  where relay HTTP answers 500 and moq-c reports `OLD` today. Both stay as wire
  stream errors, and `Old` still aborts a cut group mid-read.
  The Impact bullet says how to migrate each affected package.
- A one-range subscription for a cached live group missing the frames its
  range starts at needs a head fill. Frame bounds
  ([moq-dev/moq#6129](https://github.com/moq-dev/moq/pull/6129)) hand such a
  group out positioned at the asked frame, so reading the missing head returns
  `Error::Lagged` rather than the group being skipped (maintainer,
  2026-10-20). Only `fetch_group` requests a head fill today, so the one-range
  read has to request it instead of letting `Lagged` reach the caller.
- Benchmark filling beside `track_range_serving`, which covers serving.
