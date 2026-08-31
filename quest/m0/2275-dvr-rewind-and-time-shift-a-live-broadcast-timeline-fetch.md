# [L] DVR: rewind and time-shift a live broadcast (timeline + FETCH exist, durable storage doesn't)

## Goal

Implement and verify the behavior tracked in [#2275](https://github.com/moq-dev/moq/issues/2275)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

Rewind / time-shift / instant replay: let a viewer watch behind the live edge and scrub back. Every segmented protocol (HLS/DASH) gets this for free from segments on disk and pays seconds of latency for it. WebRTC and SRT cannot do it at all. MoQ could offer sub-second live *and* a scrubbable buffer on one connection, which nothing else does.

The good news: the **addressing layer is done and shipping**. The gap is storage and the client read side.

#### What exists today

- **Timeline track** gives time → group. `hang::catalog::Timeline { track, timescale, wall }` (`rs/hang/src/catalog/timeline.rs`), records `hang::timeline::Record { group, pts }` (`rs/hang/src/timeline.rs`) on a `moq_json::stream` track, produced/consumed via `moq_mux::timeline::{Producer, Consumer, Entry}` (`rs/moq-mux/src/timeline.rs`). `DEFAULT_GRANULARITY` = 1s.
- **FETCH** gives group → bytes. `track::Consumer::fetch_group(sequence, Option<group::Fetch>)` (`rs/moq-net/src/model/track.rs:1292`), served via `track::Producer::dynamic()` → `Dynamic::poll_requested_group` → `GroupRequest::accept`.
- **`moq-hls` proves the pair end to end** (`rs/moq-hls/src/export/`): subscribe to catalog + timeline only, render playlists from timeline records, and FETCH the covering groups from the relay cache only when an HTTP client asks for a segment. Zero standing media traffic.

So time → group → bytes already works. What's missing is that the bytes aren't there.

#### What blocks it

1. **No durable store. This is the fundamental one.** `cache::Pool` (`rs/moq-net/src/model/cache.rs`) is memory-only, and it is the *only* backing store in the tree. `Error::Evicted` (`rs/moq-net/src/error.rs:104`) correctly says an evicted group "can be re-fetched", but only from an upstream whose own retention is `track::Info.cache`  -  **`DEFAULT_CACHE` is 5 seconds** (`rs/moq-net/src/model/track.rs:31`). Past that it's `Error::NotFound`, permanently. 5s is the DVR depth of the entire system. `moq-hls`'s 16s `--window` is already a hostage to it.
2. **LRU actively targets exactly the data a rewind wants.** `TrackState::pin_latest` (`track.rs:~490`) pins one group per track (the live edge). Everything behind it is evictable, and a rewinding viewer's reads are by definition the coldest in the pool.
3. **`Info.cache` is publisher-fixed and immutable**, and `Info::clamp_stale` only clamps *down*. A subscriber cannot ask for deeper retention.
4. **The subscription aggregate is a union across subscribers.** `Subscription::poll_combined` uses `min_some(group_start)`, so one viewer seeking to group 5 drags the whole upstream aggregate back to 5 for everyone. No per-subscriber window isolation.
5. **JS has no read side at all.** No `Timeline.Consumer` in `js/hang` (producer only: `js/hang/src/container/timeline.ts`). No subscription `startGroup`/`endGroup` (Rust-only, `rs/moq-net/src/lite/subscribe.rs:21`). And the *local* `fetchGroup` fallback (`js/net/src/broadcast.ts:108`) is literally forward-only: it subscribes and scans forward, throwing `group not found` once it overshoots.
6. **Consumers can't tell a seek from a restart.** `js/hang/src/container/consumer.ts:322` ("Only a group newer than the active one can rewind the timeline"), `:397`, `:499`; `rs/moq-hls/src/export/timeline.rs:~88` warns "timeline jumped backwards; resetting the playlist window". A seek looks identical to a publisher restart to every one of these.
7. **FETCH is one whole group, no range and no frame offset.** `lite::Fetch { broadcast, track, priority, group }` (`rs/moq-net/src/lite/fetch.rs`). Seeking N seconds back is N/group\_duration serial round trips (`moq-hls`'s `for sequence in start..end` loop is exactly that). Note the IETF adapter's `FetchType::Standalone { start: Location, end: Location }` (`rs/moq-net/src/ietf/fetch.rs`) *does* carry a range  -  moq-lite doesn't.

#### Proposed shape

Roughly in dependency order; (1) is the only hard part.

1. **A persistence tier behind the pool.** Some `Store` trait the origin can consult on a cache miss before returning `NotFound` (disk, then object storage). Decide whether it's a `cache::Pool` L2 (transparent, `fetch_group` just works and the existing `Evicted` → re-fetch path covers it) or a separate `moq-store` crate a publisher opts into. Retention becomes a duration/byte budget independent of `Info.cache`.
2. **Let a subscriber discover the available window** rather than guessing. `Info.cache` advertises the live-edge TTL; a DVR window needs its own advertisement (earliest available group, per track), probably alongside the timeline.
3. **Pinning/eviction policy that doesn't fight rewind**  -  at minimum, don't let a DVR read count as a cold LRU touch.
4. **JS read side**: `Timeline.Consumer` in `js/hang`, `startGroup`/`endGroup` on the JS subscription, and a real `fetchGroup` that doesn't scan forward.
5. **A seek vs restart distinction** in the container consumers, so a backward jump can be intentional.
6. **Player API**: seek/scrub on `<moq-watch>`, plus the live-edge-relative position.

Optional and separable: a **group range on FETCH** (mirror the IETF `start`/`end`) to collapse the N-round-trip seek. That's a wire change and can land later.

#### Branch

The store, the JS read side, and the player API are additive → `main`. A FETCH range is a moq-lite wire change → `dev`, and needs `drafts/draft-lcurley-moq-lite.md` in the same PR. Widening the retention advertisement is likely wire too.

#### Cross-package sync

`rs/moq-net` ↔ `js/net`; `rs/hang` ↔ `js/hang`; `doc/concept`. If FETCH changes shape, the moq-lite draft.

#### Notes

- Worth deciding up front whether DVR is a *relay* feature (the relay keeps a window for everyone) or a *publisher/origin* feature (the origin persists and serves FETCH from storage). The relay is supposed to stay media-agnostic, and it can: this is all group-level, no media parsing. But an unbounded relay-side window is a very different memory/cost story than an origin-side one.
- `moq-hls` is the reference consumer. If DVR lands, `moq-hls` should get a real sliding-window-plus-history playlist off the same primitive rather than its own 16s cap.

## Closes

- [#2275](https://github.com/moq-dev/moq/issues/2275) - close this issue when the quest finishes
