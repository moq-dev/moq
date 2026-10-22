# moq.dev in the monorepo

## Goal

moq.dev's three sites, the moq.dev Astro blog and demos, moq.pub, and
moq.watch, live in this repository under `site/` and build against the
in-tree `@moq/*` packages. A player fix and the site change that uses it land
together, and the site stops waiting on npm releases.

## Plan

Decided 2026-10-20 in planning: move all three sites, not only the player
sites, because they share one build, one deploy, and one set of packages.
The work splits into a size cap that lands first, the move, and a follow-up
that settles which demos survive.

This epic's own remaining work: once the move lands and live deploys come from
this repository, archive moq-dev/moq.dev with a README pointing at `site/`.
Its history stays there, since the move is a fresh snapshot.

## Required

- [Cap committed binary size](/quest/m2/site/binary-size.md) - the merge queue rejects any binary a PR adds or grows past 500 KB
- [Move moq.dev into site/](/quest/m2/site/move.md) - the three sites build, check, and deploy from `site/` against the in-tree packages
- [Resolve demo and site overlap](/quest/m2/site/demo-overlap.md) - one surviving page per demo, duplicates deleted

## Related

- [Hosted worklet adoption](https://github.com/moq-dev/moq.dev/blob/main/quest/m0/dogfood-assets.md) - moq.dev's half, which the move ports back into this tree
