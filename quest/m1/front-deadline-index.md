# [M] Front deadlines scale with the touched track

## Goal

A relay front's per-event cost no longer grows with the number of tracks it
holds. Today `front.rs` `next_deadline` scans every track to find the next
linger expiry, and `serve_front`'s per-wake poll in `origin.rs` scans every
track too, so each event on one track costs O(tracks) and a broadcast that
churns track names pays O(tracks²). After this, finding the next expiry and
reacting to one track's event touch only that track.

## Plan

- Benchmark first, in `rs/moq-net/benches/origin.rs`: churn tracks through one
  front, swept over tracks per front and readers per track, so today's slope
  shows before the fix and the fix's flatness after.
- Keep parked tracks in an expiry index ordered by `since + linger` (a
  `BTreeMap` or a heap with lazy deletion), updated on every
  `Parked`/unparked transition, so `next_deadline` is its first entry and the
  deadline sweep pops only what expired.
- Replace the driver's scan-every-track poll with per-track wakes, so an
  event on one track polls that track. This also removes the quadratic
  rescan in `serve_front`'s `kio::wait`, where each pass handles one ready
  track query and then rescans every track: a route swap measured 186 µs at 32 tracks, 769 µs at
  64, and 2.16 ms at 128 (#4995's `origin/copy_walk`), which is this fix's
  regression bench.
- Keep the front's exhaustive walk test passing, and add a unit test that the
  index agrees with a full scan across random transitions.
- Not in scope: kio's level-only demand, which lets a reader that comes and
  goes between polls skip restarting the linger. It costs one extra source
  request, so it doesn't justify a kio generation counter (decided
  2026-09-28).

Public API: none. Wire: none.

## Related

- [Route wakes](/quest/m1/route-wakes.md) - narrows which fronts a route change wakes, in the same origin driver; one owner of `origin.rs` at a time
- [Front parking](/quest/m1/origin-front-parks.md) - also changes what a front holds, and wants a churn benchmark over requesters
