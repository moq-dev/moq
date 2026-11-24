# [M] JS origin listings update per changed path

## Goal

Each update to an `@moq/net` origin listing (`announced()`, `routes()`) costs
work for the paths the change touched, not a rebuild that checks every route
against every scope member.

## Plan

`#runAnnounced` in `js/net/src/origin.ts` calls `#listed` on every wake.
`#listed` walks every route in the snapshot, and each candidate scans the
scope's members (`advertReaches`, `advertOverlaps`, `claimed`). The
`producer-scopes` row of `js/net/bench/broadcasts.ts`, with one scope member
per route, measured 22 ms per update at 128 routes and 128 members in
[moq-dev/moq#9251](https://github.com/moq-dev/moq/pull/9251), as it did with
pattern scopes before.

- Rebuild only the paths a change touches, keeping the diff against `active`.
- Look members up through `Path.Prefixes`' sorted order instead of scanning
  them, as `matches` and `overlaps` already do.
- Sweep the bench over routes and scope members, and report before and after.

Public API: none. Wire: none.

## Related

- [JS announced coalescing](/quest/m3/js-announced-coalesce.md) - the same route stream's pending events
