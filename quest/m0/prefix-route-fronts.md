# [S] Prefix routes mint bounded fronts

## Goal

A peer requesting arbitrary paths under a prefix route cannot make an origin
mint one front, driver task, and upstream request per distinct path. The
number of fronts a prefix route can create is bounded, and a request past the
bound is refused rather than queued.

## Plan

Found while landing shared fronts (#4922): viewers now share one front per
path, but an optimistic resolve under a prefix route still mints a front for
every distinct requested path while the route stands. That cost is per path,
not per viewer, so a single session can amplify it by naming paths.

- Decide the bound (per route, per session, or both) and what a refusal looks
  like on each wire version; reuse the refusal shapes
  [Request caps](/quest/m0/request-caps.md) settles.
- Cover it with a test that requests many distinct covered paths and checks
  the front count stays bounded, and extend the origin benchmark with a
  distinct-path axis.

## Required

- [Request caps](/quest/m0/request-caps.md) - settles the refusal shapes reused here
- [Shared fronts](/quest/m0/shared-fronts.md) - #4922 provides the per-path front model
