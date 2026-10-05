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

- Since #4741, `main` does the opposite. A front records each refuser in its
  `refused` set (`rs/moq-net/src/model/front.rs`), and `best_route`
  (`rs/moq-net/src/model/origin.rs`) skips those entries and re-selects,
  which can reach a sibling or a shorter prefix. `js/net/src/origin.ts` keeps
  the same `refused` set per slot. Delete the re-selection in both languages,
  so the front ends with the refusal's typed code.
- Keep resume across routes that did not refuse: a route that goes away
  mid-track is not a refusal, and any covering route still resumes it.
- Tests in both languages: a refusal from the only advertiser of the longest
  prefix ends the request even when a sibling or a catch-all could serve it.
  Replace any test that asserts the fall-through.

Public API: none. Wire: none; the relay matches what the drafts specify.

## Related

- [Shared fronts](/quest/m0/shared-fronts.md) - also changes what a front holds
