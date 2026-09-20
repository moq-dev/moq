# m1: the dev line

## Goal

Work intended to land on `dev` before it merges to `main`, or be explicitly
deferred by the maintainer: the breaking
API and wire changes (the announce and wildcard surface, error codes, the
allocator mirrors, the bindings), the merge gates, and the merge itself.

## Plan

Branch a quest from `dev` when it breaks a published API or wire. A quest
stays here only if it breaks a published API or wire, or gates the merge;
[Merge dev](/quest/m1/merge-dev.md) names the gates, and [Release](/quest/m1/release.md)
names what gates the release after it. Work that is identical
on `main`, additive, or targets a `0.0.x` crate lives in
[m2](/quest/m2/README.md) even when it builds on dev-only code; it starts on
`main` after the merge. The four 0.0.x media crates are the explicit exception:
their pre-0.1 contracts live in [m0](/quest/m0/README.md) and can land on main
without the dev merge. The 2026-09-12 grooming applied that rule to every
quest here and merged main into dev. The auth API line is here for its
request-side break (`mtls=<identity>` and the now-required fields) and ranks
first because moq.pro adopts the release only once that contract is settled;
it is priority, not a merge gate, and [Merge dev](/quest/m1/merge-dev.md)
does not require it. The transport line in m2 assumes the single noq stack.

## Quests

- [Bindings announce match](/quest/m1/api-origin-scopes.md) - every binding takes a pattern scope and reports the announce match with its captures
- [PathPrefixes](/quest/m1/api-path-prefixes.md) - the unused moq_net::PathPrefixes type is deleted before the release
- [Rendition ownership](/quest/m1/api-mux-rendition.md) - one handle publishes a media track and reports its estimate, instead of five
- [Cluster -01](/quest/m1/cluster-01/README.md) - rs/moq-net and js/net speak the revised cluster extension (HOP_ID, REQUEST_UPDATE repricing) and -01 is published
- [API review gate](/quest/m1/api-review-gate.md) - each `api-*` quest above is landed or deferred by the maintainer before the merge PR opens
- [Merge dev](/quest/m1/merge-dev.md) - dev lands on main with a closing keyword for every issue it fixed
- [Release](/quest/m1/release.md) - the release moq.pro adopts: binding parity, an upgrade page, and a staging soak gate it rather than the merge
