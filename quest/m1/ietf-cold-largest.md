# [M] A relay reports its upstream's largest

## Goal

On drafts 14 to 19, a relay's SUBSCRIBE_OK reports the larger of its
upstream's Largest Location and the largest object it has received for the
track, including when it has cached nothing yet. A relative joining FETCH
(start 0) through a cold relay then gets the head of the current group,
filled upstream if needed, instead of INVALID_RANGE.

On lite-07 a relay's SUBSCRIBE_START reports the same maximum, so a relay
whose recreated copy holds only older groups never makes a downstream copy
see a false regression and end with UNROUTABLE (accepted in #5057).

## Plan

Triaged from Fastly's moq-relay-interop report (2026-09-23 run, item 14)
on 2026-10-07. The report's DOES_NOT_EXIST was fixed by #4253: the empty
join is INVALID_RANGE now, which the draft requires when no Largest was sent.
What remains is that the Largest is lost. d18 §10.2.11 and d19 §10.2.16
say "A relay MUST set LARGEST_OBJECT to the largest of" the upstream's
LARGEST_OBJECT and the largest object received upstream, whatever the
cache holds; d14 to d17 state no rule, so follow the same one there.

- The publisher's SUBSCRIBE_OK carries `live_edge(&cache).largest`
  (`rs/moq-net/src/ietf/publisher.rs`, around lines 664 and 717), which is
  `None` until a group is cached, so `(Filter::NextObject, None)` becomes
  `Joined::Empty`.
- The subscriber passes its upstream's Largest only to
  `track.start_at(largest.group)` and its held subscription state
  (`rs/moq-net/src/ietf/subscriber.rs`, around line 2013), and does not
  record the object. `live_floor` (`model/track.rs`) is a cache-validity
  group floor, not the upstream's Largest.
- The latest group is protected from eviction while the track has a
  producer, but `live_edge` falls back through the cache when the newest
  group has no object yet. An empty newer group demotes the last
  object-bearing group, which can then expire, losing its Location.

Decided 2026-10-07: carry the upstream's Largest into the model, and keep a
high-watermark of the largest object received that expiry does not lower,
so the publisher reports the maximum of the two. Serve the joining FETCH
through the existing one-group upstream fill. Rejected: leaving
INVALID_RANGE, which is compliant but loses late joiners' first group head.
This reports state learned from the upstream; nothing waits on a peer. The
same Largest goes out in every response this relay sends that carries one:
SUBSCRIBE_OK and the track-update REQUEST_OK. Inbound PUBLISH is refused
today (`run_publish_stream`), so it is out of scope.

Decided 2026-10-08: moq-lite folds in here, as one model change with two
publishers. A lite relay's SUBSCRIBE_START takes its largest from `poll_live`,
which returns `TrackState::largest()`, the newest visible cached group
(`model/track.rs` ~1422 at time of writing); TRACK_INFO carries none. The
lite publisher swaps that accessor for the same maximum at its two START
sites. The high-watermark dies with the copy and its withheld cache:
otherwise a relay re-reports a restarted publisher's old group and hides the
regression a downstream copy checks for.

Scrutinize the model change: keep any new track state crate-private unless a
consumer needs it.

Test: through a cold relay, a d16 subscriber with a relative joining FETCH
(start 0) receives the current group from object 0, and its SUBSCRIBE_OK
carries the upstream's Largest. On d18 and d19, a relay whose cached
objects are behind the upstream's Largest reports the upstream's, and one
whose cache is ahead reports its own. Regression for the high-watermark:
create an empty newer group, expire the older object-bearing group it
demoted, and SUBSCRIBE_OK still reports that group's received Location.

On lite-07, a relay whose recreated copy holds only groups below its
upstream's largest reports the upstream's in SUBSCRIBE_START, and a copy
withheld after a regression reports nothing from the old instance.

Public API: none expected. Wire: none new.

## Related

- [moq-transport ranges](/quest/m1/subscribe-ranges/ietf.md) - the same upstream fill path, for multi-group ranges
- [Fetch without SUBSCRIBE](/quest/m1/ietf-fetch-only.md) - the other FETCH gap at a relay
