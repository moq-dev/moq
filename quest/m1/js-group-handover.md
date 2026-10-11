# [L] JS track handover

## Goal

A `js/net` track subscription survives its broadcast's route swapping to
another provider with the same epoch, such as a relay migration after a
GOAWAY, and resumes from the new provider at the first frame it has not
delivered. A viewer at the live
edge with no latency budget never loses a group across the swap, and never
has to notice the swap to keep reading.

Done when `test/drain` passes with the viewer's `MAX_DELAY` at zero, the
viewer subscribes once instead of following `request.active`, and the run is
stable enough for the nightly.

## Plan

Today the origin (`js/net/src/origin.ts`) holds the outranked route until the
new one answers, then closes the old front. Every track read through that
front ends, and the app has to subscribe again on the new broadcast. Once the
old relay drops its upstream pull, the new relay subscribes upstream from
scratch at the live edge. A group boundary that lands inside that window loses
the group in flight: about 1 run in 5 at 100 ms groups.

Current Rust (`model/resume.rs`) reads route copies directly through each
reader. It retains replaced copies for unread and in-flight groups, bounds
them at replacement, and recovers a group from the new copy at the first
missing frame and byte. It does not copy through the earlier single-writer
pump or immediately cancel every old copy. Decided in the 2026-10-10 audit:
mirror the current identity, reader, recovery and lifetime contract, including
the late-duplicate correction, rather than porting the obsolete pump.
Things to settle along the way:

- Whether the request's `active` broadcast stays the same object across a swap,
  with its tracks re-sourced underneath. That is the Rust behavior and the
  simplest for players. It is also a behavior change for code that watches
  `active` to resubscribe.
- The resumed subscription names where it left off (group and frame) so the
  new provider serves the rest from its cache or upstream instead of starting
  at its own live edge.
- Failover compatibility: Rust refuses to resume onto a source whose track
  properties differ (timescale, retention, priority, order). Match it.
- Resume only between routes with the same epoch. Any other swap is a new
  broadcast that announce consumers see as a `Restart`, and players follow
  it with a fresh subscription. A route without an epoch never resumes, as
  in Rust, so `drain.ts`'s publisher mints an epoch, and the
  [transport upgrade](/quest/m1/transport-upgrade/js.md) test publishes
  under one too.
- `js/watch` and `js/hang` consumers that re-subscribe on `active` changes.
  Check whether they still need to.
- Giving up a resumed group no route continues. Mirror Rust's rule from
  [One max_age meaning](/quest/m1/cache-max-age.md): give it up once its
  wall-clock age since its successor arrived, or its media-time drift,
  reaches the reader's budget.

Add unit coverage at the origin level against stand-in sessions, then flip
`test/drain` to zero budget (drop the resubscribe loop in `drain.ts` and the
budget note in its README).

Decided 2026-10-08: Restart and One max_age meaning land first, so this
mirrors their final shape instead of chasing it.

Public API: likely a behavior change to `Origin.Requesting.active` and track
subscriptions across a swap. Report it in the PR.

## Required

- [One max_age meaning](/quest/m1/cache-max-age.md) - the rule that gives up a resumed group no route continues
- [Resume duplicates](/quest/m0/broadcast-epoch/resume-duplicates.md) - the corrected once-only delivery contract for reordered copies

## Related

- [Retired requests](/quest/m0/broadcast-epoch/retired-requests.md) - old handles cannot open unpinned requests after an identity change
- [SUBSCRIBE_DROP](/quest/m1/subscribe-drop.md) - a terminal group disposition stops recovery even for the newest group
- [FETCH max-delay](/quest/m1/fetch-max-delay.md) - recovery fetches retain the reader's budget
