# [L] Fetch as a range

## Goal

`fetch_group` is gone: every caller reads a past group through a one-range
subscription, which ends once its range is accounted for. That covers the
relay, moq-archive, hls, moq-ffi and every binding wrapper with its `doc/lib`
page, and `moq fetch` through `moq_relay::fetch_group`.

## Plan

Split out of dynamic range requests, which replaced `requested_group` with
`requested_fill`, by the maintainer on 2026-10-20
([moq-dev/moq#6123](https://github.com/moq-dev/moq/pull/6123)), so each PR
breaks one API once. Resized from [M] to [L] on 2026-10-24, when the first
attempt ([moq-dev/moq#6722](https://github.com/moq-dev/moq/pull/6722)) found
the questions decided below and landed only the `track_range_filling` benchmark.
The second attempt ([moq-dev/moq#7545](https://github.com/moq-dev/moq/pull/7545))
landed the bounded-read end on a plain cursor, the parking wire publishers, and
the cached-backfill rule; what remains is below.

- **Ending a bounded read** (decided 2026-10-20). `recv_group` returns `None`
  once every sequence in a bounded window is delivered, dropped, stale, or
  unfillable. Landed for a track's own cache: a `with_range` cap never ends a
  read, since it may rise again. A front's logical track (a routed track,
  `model/resume.rs`) still parks, because each copy's reader parks and the
  routes judge the end. Its `Delivered` counts what is no longer owed as
  handed out (`track::Subscriber::skip_ordered`), so it cannot account for the
  window alone, and `resume_model` asserts a reader never ends before a route
  declares an end, which ending there has to change. `Ok(None)` before any group arrives is a miss, which the
  moq-ffi fetch wrapper reports as `NotFound`; after one, it is the range's
  end. A one-range read surfaces no `NotFetchable` or `Old` result, and a
  peer's reset with either names the group dropped. Every fetch boundary
  already treats `NotFetchable` as not found (relay HTTP 404, hls
  `is_cache_miss`, moq-ffi `map_fetch_error`, moq-c `NOT_FOUND`); a fetched
  `Old` becomes a miss too, where relay HTTP answers 500 and moq-c reports
  `OLD` today. Both stay as wire stream errors, and `Old` still aborts a cut
  group mid-read. The Impact bullet says how to migrate each affected package.
- A one-range subscription for a cached live group missing the frames its
  range starts at needs a head fill. Frame bounds
  ([moq-dev/moq#6129](https://github.com/moq-dev/moq/pull/6129)) hand such a
  group out positioned at the asked frame, so reading the missing head returns
  `Error::Lagged` rather than the group being skipped (maintainer,
  2026-10-20). Only `fetch_group` requests a head fill today, so the one-range
  read has to request it instead of letting `Lagged` reach the caller.
  `fill::Ask` already carries a start frame, so the miss judge can count a
  headless slot as a hole for a bounded window.
- **Wire publishers keep parking** (landed). Every wire publisher reads its
  subscription through `track::Subscriber::parked`: the IETF publisher and
  lite publishers before lite-07 keep parking on a bounded downstream
  SUBSCRIBE rather than FIN it, and a lite-07 publisher FINs a covered bounded
  subscription without SUBSCRIBE_END
  ([moq-dev/moq#6807](https://github.com/moq-dev/moq/pull/6807)) until the
  explicit end below replaces that FIN.
- **Route recovery keeps a frame start.** `resume::Recover` fetches the rest of
  a group from frame N on the new route (`group::Fetch::frame_start`), which a
  whole-group range cannot say, so a crate-private fetch stays for it.
- **Ends are signaled, not inferred** (maintainer, 2026-11-01, on
  [moq-dev/moq#7545](https://github.com/moq-dev/moq/pull/7545)): "we should
  have a SUBSCRIBE_END message or something to signal the last group of a
  track? we were using QUIC FIN to signal that but that prevents updates in the
  future, so we should have a signal instead". Two FINs end a lite
  subscription today: the one after SUBSCRIBE_END on lite-05 and later, and
  lite-07's covered bounded FIN without SUBSCRIBE_END. Either way, a later
  SUBSCRIBE_UPDATE can no longer widen the subscription. The direction is an
  explicit signal that names the end and leaves the stream open. It does not
  yet say which message carries a covered subscription's end, or whether
  SUBSCRIBE_END stops closing the stream. Both are open on
  [moq-dev/moq#7545](https://github.com/moq-dev/moq/pull/7545), and the wire
  half belongs to [lite-07 ranges](/quest/m1/subscribe-ranges/lite.md).
- **A miss with no frontier** (maintainer, 2026-10-25). A reader asks fills
  only below the newest group the live feed produced (decision 2 on
  [moq-dev/moq#6123](https://github.com/moq-dev/moq/pull/6123)), and
  `Misses::judge` asks nothing while `newest_produced` is unset. So on a relay
  copy that no subscription feeds, `Range::groups(5..6)` asks no fill and
  parks forever. While the track has no live feed, holes in a bounded range
  are misses up to its end. The rejected options were treating every bounded
  range's holes as misses, which asks fills for groups a live feed has yet to
  produce, and keeping a by-sequence read beside subscriptions. Treating "no
  group produced" as "no feed" asks too early: on lite-06,
  `subscribe_ranges::sparse_ranges_reach_the_publisher_on_lite07` then reads
  `[0]`. moq-archive needs no new signal. Its consumer creates a track in the
  same refresh that pushes the newest stored sequence and declares it with
  `start_at`, so only an empty index has no frontier there. Which explicit
  signal stands in for the frontier is open on
  [moq-dev/moq#7545](https://github.com/moq-dev/moq/pull/7545).
- **Holes below where the feed started.** On a live track with no declared
  start, a hole below the feed's first arrival is waited for over the arrival
  grace (`Ledger::folds_at`) before it is asked, where `fetch_group` asks at
  once. With no budget and no publisher max age the grace is unbounded, so the
  read parks for good (the `rejoin` failures on
  [moq-dev/moq#7487](https://github.com/moq-dev/moq/pull/7487)). A hole under a
  declared start is asked at once, but a refusal there is final only once the
  ledger gives up on it. Open on
  [moq-dev/moq#7545](https://github.com/moq-dev/moq/pull/7545).
- **Cached backfill is owed** (maintainer, 2026-10-25; landed). A cached
  backfill group inside a reader's window is taken as filled, whether or not
  the live feed may still deliver it, so a relay behaves the same warm or
  cold. Re-filling a cached group was rejected: it costs an upstream FETCH for
  a group already in the cache.
- **Relay demand before lite-07** (maintainer, 2026-10-25). A one-range read's
  ranges join the track's aggregate, so a relay would put a bounded SUBSCRIBE
  upstream where `fetch_group` puts a FETCH. `fetch_only.rs` asserts that a
  moq-transport fetch never subscribes, and a lite SUBSCRIBE before lite-05
  cannot name a missing group. A range wholly below the live edge stays out of
  the upstream SUBSCRIBE on any wire without range SUBSCRIBE and is filled by
  FETCH, keeping today's wire behavior: since
  [moq-dev/moq#6724](https://github.com/moq-dev/moq/pull/6724) a run of misses
  is one range FETCH on moq-transport, and per-group FETCH is lite-only, until
  [lite-07 ranges](/quest/m1/subscribe-ranges/lite.md) land. A bounded
  upstream SUBSCRIBE accounted by SUBSCRIBE_START and SUBSCRIBE_DROP was
  rejected: it works on lite-05 and later only. A subscription to a copy no
  SUBSCRIBE accepts waits for the track's info today, so this also needs a
  one-range read to resolve from the info its fill installs.
- **The fill path goes linear first** (maintainer, 2026-10-25). One-range
  reads put every past read on the fill path, which `track_range_filling`
  measured as superlinear in range count and span. It is flat since
  [moq-dev/moq#6995](https://github.com/moq-dev/moq/pull/6995).
