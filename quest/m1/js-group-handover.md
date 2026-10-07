# [L] JS track handover

## Goal

A `js/net` track subscription survives its broadcast's route swapping to
another provider, such as a relay migration after a GOAWAY, and resumes from
the new provider at the first frame it has not delivered. A viewer at the live
edge with no latency budget never loses a group across the swap, and never
has to notice the swap to keep reading.

Done when `test/drain` passes with the viewer's `MAX_AGE` at zero, the
viewer subscribes once instead of following `request.active`, and the run is
stable enough for the nightly.

## Plan

Today the origin (`js/net/src/origin.ts`) holds the outranked route until the
new one answers, then closes the old front. Every track read through that
front ends, and the app has to subscribe again on the new broadcast. Once the
old relay drops its upstream pull, the new relay subscribes upstream from
scratch at the live edge. A group boundary that lands inside that window loses
the group in flight: about 1 run in 5 at 100 ms groups.

Rust solves this with #4741's single-writer pump (`model/resume.rs`): each track a
front serves is one producer, a route change subscribes the new route from
the first frame the logical track lacks (mid-group), an open group is
continued in place, duplicates are dropped by frame index, and the old route
is cancelled once the new one feeds the track. Mirror that shape and naming
in JS rather than inventing a second model. Things to
settle along the way:

- Whether the request's `active` broadcast stays the same object across a swap,
  with its tracks re-sourced underneath. That is the Rust behavior and the
  simplest for players. It is also a behavior change for code that watches
  `active` to resubscribe.
- The resumed subscription names where it left off (group and frame) so the
  new provider serves the rest from its cache or upstream instead of starting
  at its own live edge.
- Failover compatibility: Rust refuses to resume onto a source whose track
  properties differ (timescale, retention, priority, order). Match it.
- `js/watch` and `js/hang` consumers that re-subscribe on `active` changes.
  Check whether they still need to.
- Giving up a resumed group no route continues. Mirror Rust's rule from
  [One max_age meaning](/quest/m1/cache-max-age.md): give it up once its
  wall-clock age since its successor arrived, or its media-time drift,
  reaches the reader's budget.

Add unit coverage at the origin level against stand-in sessions, then flip
`test/drain` to zero budget (drop the resubscribe loop in `drain.ts` and the
budget note in its README).

Public API: likely a behavior change to `Origin.Requesting.active` and track
subscriptions across a swap. Report it in the PR.
