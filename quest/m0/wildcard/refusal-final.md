# [S] A standing refusal ends the front

## Goal

A refusal from the winning route is the answer. The request ends with that
refusal instead of re-selecting a sibling advertiser of the same prefix or
falling through to a shorter one, as the wildcard line, the lite draft, and
the cluster draft already say. A new route, or the refuser's route changing,
is what a client's ordinary resubscribe resolves against.

## Plan

Decided in the 2026-10-05 audit: a refusal is final, with no retry. The
maintainer: "otherwise we'll try all other possible options and cause a
cascade of requests".

- Since #4741, `main` does the opposite. The front's standing-refusal arm
  (`Err(Refusal { standing: true, .. })` in `rs/moq-net/src/model/front.rs`)
  records the refuser in its `refused` set and pushes `Reselect`, and
  `best_route` (`rs/moq-net/src/model/origin.rs`) skips those entries, which
  can reach a sibling or a shorter prefix. `js/net/src/origin.ts` keeps the
  same `refused` set per slot. Change only that arm, in both languages: it
  ends the front with the refusal's typed code instead of re-selecting.
- Keep the `refused` set itself. `source_closed` also fills it, to exclude a
  standing route whose source just ended ("asking it again would re-request
  the broadcast that just ended"); deleting the set would re-pick that route
  in a loop. Per-track refusal (`redispatch`) is already final, so only
  front-level route selection changes.
- Keep resume across routes that did not refuse: a route that goes away
  mid-track is not a refusal, and any covering route still resumes it.
- Tests in both languages: a refusal from the only advertiser of the longest
  prefix ends the request even when a sibling or a catch-all could serve it.
  Replace any test that asserts the fall-through; the tests asserting
  `refused_routes()` mix both causes and need splitting.

Public API: none. Wire: none; the relay matches what the drafts specify.

## Related

- [Shared fronts](/quest/m0/shared-fronts.md) - also changes what a front holds
