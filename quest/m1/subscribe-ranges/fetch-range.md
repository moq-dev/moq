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
landed the bounded-read end on a plain cursor, the parking wire publishers,
the cached-backfill rule, the bounded miss grace, and the unfed moq-transport
copy; what remains is below.

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
  explicit end below removes that FIN.
- **Route recovery keeps a frame start.** `resume::Recover` fetches the rest of
  a group from frame N on the new route (`group::Fetch::frame_start`), which a
  whole-group range cannot say, so a crate-private fetch stays for it.
- **Ends are signaled, not inferred** (maintainer, 2026-11-01, on
  [moq-dev/moq#7545](https://github.com/moq-dev/moq/pull/7545)): "we should
  have a SUBSCRIBE_END message or something to signal the last group of a
  track? we were using QUIC FIN to signal that but that prevents updates in the
  future, so we should have a signal instead". The follow-up answers, the same
  day:
  - "do we need a SUBSCRIBE_COVERED? We have SUBSCRIBE_DROP so the peer can
    determine that the bounded range has been received? SUBSCRIBE_END (rename
    to FINISH?) would signal the final group of the track maybe, but would be
    independent of the bounded range?" No new message: a subscriber judges a
    bounded range complete from the groups it received, SUBSCRIBE_DROP, the
    Range Starts in SUBSCRIBE_OK, and SUBSCRIBE_END. Drops and groups name
    absolute sequences, so the subscriber judges against its latest ranges and
    an update in flight changes nothing it has counted; the FIN's race with an
    unread update goes away. One gap: before SUBSCRIBE_OK a lite-07 publisher
    holds back what it gave up (`TrackRun::skipped`) and answers only once a
    group stream resolves the start, so a bounded subscription none of whose
    groups it can serve gets neither, and only the covered FIN ends it today.
    Fix: once such a subscription is covered, the publisher sends SUBSCRIBE_OK
    and then the drops it held. Datagrams are left out: they become their own
    track kind, which takes no ranges (maintainer, 2026-11-01).
  - "Keep stream open": SUBSCRIBE_END (or FINISH) names the track's final group
    and no longer FINs the Subscribe Stream, so an update can still reach an
    ended track's history. The subscriber FINs once it wants nothing more, and
    the publisher FINs in reply. The covered FIN goes too.
  - ✅ Rename SUBSCRIBE_END to SUBSCRIBE_FINISH in lite-07 (maintainer,
    2026-11-01). It matches TRACKS_FINISH and `track::Producer::finish`, and
    "END" would read as closing the stream, which it no longer does.

  The settled lite-07 wire design, together:
  - SUBSCRIBE_FINISH (formerly SUBSCRIBE_END) names the track's final group and
    no longer FINs the Subscribe Stream.
  - A covered bounded subscription is not FINed either: the subscriber judges
    it complete from the groups it received, SUBSCRIBE_DROP, the Range Starts in
    SUBSCRIBE_OK, and SUBSCRIBE_FINISH.
  - A publisher that held back SUBSCRIBE_OK (`TrackRun::skipped`) sends it, then
    the drops it held, once such a subscription is covered.
  - The subscriber FINs the stream once it wants nothing more, and the
    publisher FINs in reply.
  - Datagrams take no ranges, since they become their own track kind. The
    datagram-tracks plan
    ([moq-dev/moq#7575](https://github.com/moq-dev/moq/pull/7575)) touches
    `lite.md` too, and should take this design and the rename as given.

  This wire half belongs to [lite-07 ranges](/quest/m1/subscribe-ranges/lite.md)
  (Rust and the draft) and [JS ranges](/quest/m1/subscribe-ranges/js.md), not
  to this quest. It is recorded here while
  [moq-dev/moq#7487](https://github.com/moq-dev/moq/pull/7487) rewrites that
  quest, and moves there once it lands.
- **The live edge is told, never inferred** (maintainer, 2026-11-01, on
  [moq-dev/moq#7545](https://github.com/moq-dev/moq/pull/7545)): "If the relay
  doesn't know the live edge, it shouldn't say anything. It should be a
  separate message if it's async or the OK should block. NEVER use the cache
  as the live edge." The OK blocks, since the wire already works that way:
  lite-07 SUBSCRIBE_OK carries Largest Group and waits until each Range's
  start resolves, a relay's `TrackRun` waits on its source's start
  (`track::Subscriber::poll_start`) before answering, and an idle copy reads
  nothing until its route answers. The draft's exception for a start past the
  publisher's largest position should say that a relay knows that position
  only from its upstream's answer (lite-07 ranges' draft change). A reader
  asks fills only below the newest group the live feed produced (decision 2
  on [moq-dev/moq#6123](https://github.com/moq-dev/moq/pull/6123)), so with
  nothing produced it waits for the answer that its own demand's upstream
  subscription brings. Earlier (maintainer, 2026-10-25): while a track has no
  live feed, holes in a bounded range are misses up to its end, and treating
  "no group produced" as "no feed" asks too early
  (`subscribe_ranges::sparse_ranges_reach_the_publisher_on_lite07` reads
  `[0]` on lite-06). moq-archive needs nothing new: its consumer declares the
  newest stored sequence with `start_at`.
- **The session marks a copy unfed** (maintainer, 2026-11-01, on
  [moq-dev/moq#7545](https://github.com/moq-dev/moq/pull/7545); landed). ✅
  The session marks a copy it holds no upstream subscription for unfed
  (`track::Producer::set_unfed`), from its own state, never from the cache.
  A bounded read's holes there are misses up to its end at once, per the
  2026-10-25 rule, a refusal is final, and the read takes what the cache holds
  in its window even while the copy is idle; an open read still waits for a
  feed. The moq-transport subscriber marks a lingering copy unfed, such as one
  learned from TRACK_STATUS for a fetch, and keeps a subscription whose ranges
  all end below the publisher's reported Largest Object out of the upstream
  SUBSCRIBE (`fetch_only.rs`). The rejected alternative, a one-range read always
  subscribing so its OK names the edge, broke the fetch-only rule below.
- **Every hole ends** (maintainer, 2026-11-01, on
  [moq-dev/moq#7545](https://github.com/moq-dev/moq/pull/7545)): "uhh the
  publisher is supposed to emit the start/end bounds if the subscriber didn't
  ask for one? I don't understand, but parking forever is terrible?" A hole is
  asked as a miss, refused, or covered by the publisher's bounds, and a
  bounded read never parks for good. A hole under the start the publisher
  resolved is asked at once, and a refusal there is held while the feed may
  still deliver the group late (maintainer, 2026-10-21, on
  [moq-dev/moq#6171](https://github.com/moq-dev/moq/pull/6171)). Nothing is
  inferred from the first arrival, since a local producer may still write a
  lower group (`late_lower_group`). Landed: that hold, and the wait before a
  hole with no start under it is asked, last the arrival grace, or
  `ledger::GRACE` when neither a budget nor a max age bounds it
  (`TrackState::miss_grace`), and a held refusal wakes its reader at its
  deadline.
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
  rejected: it works on lite-05 and later only. Landed for a lingering
  moq-transport copy whose subscription lies wholly below the reported Largest
  Object (the unfed copy above). Still to do: a first request with a bounded
  subscription, which SUBSCRIBEs before any TRACK_STATUS names an edge; a
  subscription only partly below the edge; and lite before lite-07. A
  subscription to a copy no SUBSCRIBE accepts waits for the track's info
  today, so this also needs a one-range read to resolve from the info its fill
  installs.
- **The fill path goes linear first** (maintainer, 2026-10-25). One-range
  reads put every past read on the fill path, which `track_range_filling`
  measured as superlinear in range count and span. It is flat since
  [moq-dev/moq#6995](https://github.com/moq-dev/moq/pull/6995).
