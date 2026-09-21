# dev

## Goal

The published API and wire breaks, landed together: every quest here merges
into `dev`, and `dev` merges into `main` once the line is empty.

## Plan

A quest belongs here only if it breaks a published API or wire, or gates the
merge; [Merge dev](/quest/dev/merge-dev.md) names the gates and
[Release](/quest/dev/release.md) what gates the release after it. Work that is
additive, identical on `main`, or targets a `0.0.x` crate lives under
[main](/quest/main/README.md) or [next](/quest/next/README.md), even when it
builds on dev-only code. The auth API line ranks first because moq.pro adopts
the release only once that contract is settled; it is priority, not a merge
gate.

## Quests

- [Bindings announce match](/quest/dev/api-origin-scopes.md) - every binding takes a pattern scope and reports the announce match with its captures
- [Rendition ownership](/quest/dev/api-mux-rendition.md) - one handle publishes a media track and reports its estimate, instead of five
- [Cluster -01](/quest/dev/cluster-01/README.md) - rs/moq-net and js/net speak the revised cluster extension (HOP_ID, REQUEST_UPDATE repricing) and -01 is published
- [API review gate](/quest/dev/api-review-gate.md) - each `api-*` quest above is landed or deferred by the maintainer before the merge PR opens
- [Merge dev](/quest/dev/merge-dev.md) - dev lands on main with a closing keyword for every issue it fixed
- [Release](/quest/dev/release.md) - the release moq.pro adopts: binding parity, an upgrade page, and a staging soak gate it rather than the merge
