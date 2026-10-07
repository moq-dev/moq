# [S] IETF subscriber opts out of duplicate properties

## Goal

Our IETF subscriber learns a track's properties once, from TRACK_STATUS, and
from draft 20 opts out of the copy in SUBSCRIBE_OK and FETCH_OK with
INCLUDE_PROPERTIES = 0, without losing properties or object timestamps
against a publisher that refuses TRACK_STATUS.

## Plan

Decided (2026-10-01), carried over from fetch-only demand (#4974): send
TRACK_STATUS in parallel with every SUBSCRIBE or FETCH, set
INCLUDE_PROPERTIES to 0 on those from draft 20, and ignore the duplicate
before draft 20. The publisher half already landed there: our publisher
serves TRACK_STATUS, and the opt-out no longer strips object timestamps.
Fetch-only demand already asks TRACK_STATUS instead of subscribing.

Open, for the maintainer (found 2026-10-07): every released moq-rs
publisher, drafts 20-22 included, refuses TRACK_STATUS with NOT_SUPPORTED,
and on INCLUDE_PROPERTIES = 0 strips the Timestamp from every object. A
subscriber that opts out against one loses max cache duration, priority,
group order, and timestamps on a published version. Each SUBSCRIBE also
gains a request, which spends request-ID credit. Options:

- Opt out only once this session's peer has answered a TRACK_STATUS, so the
  first request on a session pays the duplicate.
- Opt out unconditionally, once releases that serve TRACK_STATUS are old
  enough to drop.
- Drop the opt-out: SUBSCRIBE_OK and FETCH_OK keep carrying the properties,
  and TRACK_STATUS stays for fetch-only demand. Then delete this quest.

Test TRACK_STATUS plus the opt-out with source timestamps that differ from
arrival, and against a publisher that refuses TRACK_STATUS.

## Related

- [FETCH_OK properties](/quest/m1/fetch-ok-properties.md) - the FETCH_OK half of the same properties
- [Untimed model](/quest/m1/untimed-model.md) - decides what a track without declared units looks like to the subscriber
