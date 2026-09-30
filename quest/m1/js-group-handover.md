# [L] JS group-boundary handover

## Goal

A `js/net` track subscription survives its broadcast's route swapping to
another provider, such as a relay migration after a GOAWAY, and resumes from
the new provider at the first group it has not delivered. A viewer at the live
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

Rust already solves this. `moq-net`'s origin fronts are spliced broadcasts, and
`model/resume.rs` resumes each track from the replacement at the first missing
group, capping the old segment so it ends at the boundary on its own. Mirror
that shape and naming in JS rather than inventing a second model. Things to
settle along the way:

- Whether the request's `active` broadcast stays the same object across a swap,
  with its tracks re-sourced underneath. That is the Rust behavior and the
  simplest for players. It is also a behavior change for code that watches
  `active` to resubscribe.
- The resumed subscription names where it left off (the `groups` floor) so the
  new provider serves the missing group from its cache or upstream instead of
  starting at its own live edge.
- Failover compatibility: Rust refuses to splice a source whose track
  properties differ (timescale, retention, priority, order). Match it.
- `js/watch` and `js/hang` consumers that re-subscribe on `active` changes.
  Check whether they still need to.

Add unit coverage at the origin level against stand-in sessions, then flip
`test/drain` to zero budget (drop the resubscribe loop in `drain.ts` and the
budget note in its README).

Public API: likely a behavior change to `Origin.Requesting.active` and track
subscriptions across a swap. Report it in the PR.
