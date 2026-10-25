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
the open questions below and landed only the `track_range_filling` benchmark.

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
  `fill::Ask` already carries a start frame, so the miss judge can count a
  headless slot as a hole for a bounded window.
- **Wire publishers keep parking.** The lite subscriber reads an upstream FIN
  as the track's end, because "bounded downstream demand alone never FINs; the
  publisher parks" (`lite/subscriber.rs`). Once `recv_group` ends a bounded
  read, the lite and IETF publishers keep parking on a bounded downstream
  SUBSCRIBE rather than FIN it, or a peer on a published version reads the
  range's end as the track's.
- **Route recovery keeps a frame start.** `resume::Recover` fetches the rest of
  a group from frame N on the new route (`group::Fetch::frame_start`), which a
  whole-group range cannot say, so a crate-private fetch stays for it.
- **Open: a miss with no feed frontier.** A reader asks fills only below the
  newest group the live feed produced (decision 2 on
  [moq-dev/moq#6123](https://github.com/moq-dev/moq/pull/6123)). On a track
  nothing feeds live, such as the moq-archive reader's or a relay copy no
  subscription feeds, a one-range read never asks and parks forever:
  `Range::groups(5..6)` on a fill-only track asks no fill. `fetch_group`
  queues a one-group fill instead. Options: holes in a bounded range are
  misses up to its end while the track has no live feed (recommended); every
  bounded range's holes are misses up to its end, which asks fills for groups
  a live feed has yet to produce; or a by-sequence read stays beside
  subscriptions.
- **Open: cached backfill.** A range reader never reads a group another
  reader's fill cached: the arrival walk skips backfill and the miss judge
  counts it held, so a second `0..1` read after a first reader's fill cached
  group 0 parks with no fill asked. `fetch_group` reads it from the cache.
  Recommended: a cached backfill group in a reader's window is owed to it like
  its own fill's answer, as a relay behaving the same warm or cold requires.
  That retires `fetched_backfill_not_subscribed` and
  `refilled_sequence_stays_out_of_subscriptions`, which assert that a
  subscriber never sees backfill.
- **Open: relay demand before lite-07.** A one-range read's ranges join the
  track's aggregate, so a relay puts a bounded SUBSCRIBE upstream where
  `fetch_group` puts a FETCH. `fetch_only.rs` asserts that a moq-transport
  fetch never subscribes, and a lite SUBSCRIBE before lite-05 cannot name a
  missing group, so the read would park on an upstream miss. Options: a range
  wholly below the live edge stays out of the upstream SUBSCRIBE on a wire
  without range SUBSCRIBE and is filled by per-group FETCH, keeping today's
  wire behavior (recommended); or it goes upstream as a bounded SUBSCRIBE that
  SUBSCRIBE_START and SUBSCRIBE_DROP account for, lite-05 and later only.
- **The fill path is superlinear.** `track_range_filling` drains one reader's
  R disjoint ranges of S filled groups each, and the cost per group grows with
  both: at span 64, 568K groups/s over 1 range, 69K over 16, and 1.9K over 256.
  Each poll's drift edge and each candidate's successor search walk `lookup`
  over the reader's runs, and a filled group is never shown, so each passes
  every filled group; `Misses::retain` also rescans every filled group on each
  poll. One-range reads put every past read on this path.
