# [M] Verified route upgrade

## Goal

A relay whose front learned its origin from a reply moves a live subscription
to a cheaper route once that route's source proves it serves the same origin,
instead of staying on the costlier route until it fails.

## Plan

On moq-lite-07 a front's identity is the origin its source's SUBSCRIBE_OK or
FETCH_OK names, not the route's first hop, because one route can lead to many
pool members. The front only learns a new route's origin once that source
replies, so today it stays on its live source when a better route appears
(`Pin::Stay` in `rs/moq-net/src/model/front.rs`) and only moves on failover.
That keeps content from being spliced across origins but leaves traffic on a
route that may no longer be the cheapest.

The move to make: when a cheaper route appears, ask it before letting go of the
live source, and splice over at a group boundary only once its reply names the
front's origin. A reply naming another origin leaves the front where it is,
with nothing delivered from the candidate. Things to watch:

- The candidate's content is held until admitted (the copy's provenance), so
  asking early must not deliver anything or start demand the live source
  already covers longer than needed.
- Churn: a route that keeps flapping should not keep opening and dropping
  upstream subscriptions.
- Measure the cost of a verification round trip against staying put, and
  benchmark it across routes and subscribers.

## Related

- [Wildcard](/quest/m1/wildcard/README.md) - the reply-named identity landed
  there
- [Cluster origin reply](/quest/m1/cluster-origin.md) - the same identity on
  moq-transport
