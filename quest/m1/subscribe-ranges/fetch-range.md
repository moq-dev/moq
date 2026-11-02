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
copy. The third ([moq-dev/moq#7625](https://github.com/moq-dev/moq/pull/7625))
landed the end of a front's bounded read, the head fill, and the first bounded
moq-transport request; what remains is the API removal, its consumers, and the
legacy demand questions below.

- **Ending a bounded read** (decided 2026-10-20; landed). `recv_group` returns
  `None` once every sequence in a bounded window is delivered, dropped, stale, or
  unfillable, on a track's own cache and on a front's logical track alike: the
  serving copy's cursor judges the window from its resume point, a replaced or
  backfill copy that may still deliver keeps the read open, and so does a group
  in range the local floor or cap keeps out. A `with_range` cap never ends a
  read. Still to do with the API: `Ok(None)` before any group arrives is a miss,
  which the moq-ffi fetch wrapper reports as `NotFound`; after one, it is the
  range's end. A one-range read surfaces no `NotFetchable` or `Old` result, and a
  peer's reset with either names the group dropped. Every fetch boundary
  already treats `NotFetchable` as not found (relay HTTP 404, hls
  `is_cache_miss`, moq-ffi `map_fetch_error`, moq-c `NOT_FOUND`); a fetched
  `Old` becomes a miss too, where moq-c reports `OLD` today. Both stay as wire
  stream errors, and `Old` still aborts a cut group mid-read. The Impact bullet
  says how to migrate each affected package.
- **A bounded read fills a missing head** (landed). A bounded window hands out a
  cached live group missing its first frames only once a fill answered it from
  frame 0, as `fetch_group` asks, and gives the group up when none can.
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
  rejected: it works on lite-05 and later only. Landed for moq-transport: a
  copy no SUBSCRIBE feeds (the unfed copy above), and a first request whose
  ranges all have an absolute start and end, which asks TRACK_STATUS first and
  subscribes only once a range reaches the reported Largest Object.
  `range_read.rs` reads history and a range across the live edge on every
  wire, direct and relayed; draft-18 refuses the latter, having no bounded
  SUBSCRIBE (`subscribe_join`). Found migrating the moq-net integration tests
  onto one-range reads, and settled by the maintainer on 2026-11-02
  ([moq-dev/moq#7625](https://github.com/moq-dev/moq/pull/7625)):
  - **Scope.** ✅ Keep going in one PR: remove the public `fetch_group` and
    migrate its callers, finishing this quest. Alternative: land the bounded
    read changes first and remove the API in a later PR.
  - **Past-group reads on lite-05/06.** Lite before lite-07 tells no live edge
    without a SUBSCRIBE, so a one-range read of history there SUBSCRIBEs where
    `fetch_group` FETCHes: `fetch_pipeline` (the FETCH sent with TRACK),
    `fetch_fanout`, `fetch_cancel`, and `catalog_handover`'s fetch cases
    assert the FETCH.
    - ✅ (a) Accept the bounded SUBSCRIBE before lite-07 and rewrite those four
      tests.
    - (b) Serve a fully bounded absolute subscription fetch-only before
      lite-07, keeping today's wire behavior, though a range past the live
      edge ends short.
  - **Newest group on moq-transport.** A read of the group at the reported
    Largest Object reaches "the group the publisher may still be writing"
    (`wants_feed`), so it subscribes. Draft-18 refuses that, and so does a
    finished track (`fetch_only`, `compress::ietf_fetch_keeps_the_stored_form`).
    The relay's newest-group read (`moq_relay::fetch_group(None)`) reads
    exactly that group.
    - ✅ Read it as live with a SUBSCRIBE, with no special-case fallback, and
      accept the draft-18 and finished-track refusals. "Whatever the simplest
      option is, prob subscribe. I hate the IETF fetch/subscribe logic and we
      should not agonize over making it correct."
    - FETCH it.
    - SUBSCRIBE while the track is live, FETCH once it ended.
  - **Multi-group history between our moq-transport nodes.** Our
    moq-transport publishers answer only a one-group FETCH, so a run of
    missing history groups read from one of them is refused and named dropped
    (`subscribe_ranges`'s draft-22 cases open with a range reaching the
    Largest Object).
    - ✅ Accept the refusal until [hole markers](/quest/m2/ietf-hole-marker.md)
      let our publishers answer a range.
    - Answer a multi-group FETCH now.
  - Fetch-only behavior to carry over or drop: a fetch keeps both sessions busy
    (`session_idle`), a session close cancels a pending fetch from either side
    (`subscription_end_integrity`), a pre-lite-05 fetch is served from a feed
    opened with it and an unfed fetch is a miss (`fetch_from_feed`), an
    abandoned fetch reaches the publisher and a refetch within the linger stays
    on the relay (`fetch_cancel`), an untimed fetched frame arrives untimed
    (`untimed`), and a fetched datagram is `NotFetchable` (`datagram`, now a
    miss by the rule above).
- **Blockers found removing the API** (2026-11-02, on
  [moq-dev/moq#7625](https://github.com/moq-dev/moq/pull/7625)). Making
  `track::Consumer::fetch_group` crate-private, which the wire publishers
  still serve a FETCH through, and moving the relay, hls, and transcode onto
  one-range reads surfaced three gaps:
  - **Sequential one-range reads stall on lite-07.** On one session, a
    one-range read after an earlier one on the same track can time out,
    direct or relayed. Reading groups 1 then 2 stalls on 2, and 2, 1, 0 stalls
    on 0, even after the copy's linger expires. Lite-06 and moq-transport pass.
    It hangs hls's `a_live_fmp4_import_serves_segments_over_lite07`, which reads
    a segment's groups one by one. To reproduce: in `range_read.rs`'s fixture
    on lite-07, read `one_range(1..2)`, drop it, then read `one_range(2..3)`.
  - **A local track asks no fill without a told edge.** A one-range read on a
    local track asks its `Requests` handler for a missing group only below a
    start the publisher declared (`track::Producer::start_at`). Writing a
    newer group is not enough (`late_lower_group`). `fetch_group` asked at once.
    So in-process reads of on-demand groups hang: moq-c group requests and
    moq-ffi fills (`track_dynamic_serves_a_fetch_miss`,
    `track_request_dynamic_survives_accept`, `fill_goes_unwanted_*`), and
    transcode's rung, which reads its source and serves its output that way
    (`a_group_nobody_reads_releases_its_fill`,
    `retirement_waits_for_an_unclaimed_fetch`). moq-archive already declares
    its edge, but moq-c and moq-ffi expose no `start_at`.
  - **A group past the live edge waits.** A one-range read of a group the
    live track has not reached yet waits for it, where `fetch_group`'s FETCH
    was refused at once. `moq fetch --group 99` and the relay's
    `/fetch?group=99` time out (HTTP 504 after 30 s) instead of answering 404.
  - Keep the subscription open while its group is read. Dropping the
    `track::Subscriber` after `recv_group` stalls a group still being written
    on every wire, so each boundary that hands a group out (relay HTTP,
    moq-ffi, hls, transcode) keeps the subscriber with it.
- **The fill path goes linear first** (maintainer, 2026-10-25). One-range
  reads put every past read on the fill path, which `track_range_filling`
  measured as superlinear in range count and span. It is flat since
  [moq-dev/moq#6995](https://github.com/moq-dev/moq/pull/6995).
