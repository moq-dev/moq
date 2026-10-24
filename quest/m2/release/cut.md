# [M] Cut and publish the prepared breaking release

## Goal

The prepared main branch reaches release with the finalized wire and intended
package versions, and the release-triggered publication jobs complete.

## Plan

Follow CONTRIBUTING.md's release procedure: merge release into main first,
then main into release using merge commits. Confirm the maintainer's cut
instruction before executing it. Prepare the package-specific semver changes
at that time; Rust publication remains owned by release-plz. `moq-rs` is
already 0.6.0 on main and unpublished, so keep it rather than bumping again. Run candidate
compatibility, draft, package, and all-language interop checks, and verify
publication outcomes. Separate artifact install proofs and package-specific
tags follow this cut. This planning PR cuts and publishes nothing.
After publication, re-measure the planned wire-compat breaks against the
published versions, as CONTRIBUTING.md's release procedure describes.

This cut carries stats epochs, so it drops
[#4810](https://github.com/moq-dev/moq/pull/4810)'s release-only wall-clock
group seed: the merge of release into main keeps main's `rs/moq-stats` and
`doc/concept/stats.md`. MoQ Pro's VOD `storage.json` moves from the same
seed to its own epoch.

Recurs once per major release (decided 2026-10-19). The PR that finishes the
cut shifts the milestones up, as the root [questline](/quest/README.md)
describes. This epic's remaining publication and adoption quests move to m0,
the rest of m2 moves to m1, and every link to a moved path is rewritten. That
PR also removes this path from every `Required`, which releases the audits
waiting on it. It then re-creates this quest, the [API shape
review](/quest/m2/release/api-review.md), and the [feature
review](/quest/m2/release/feature-review.md) at the same paths under a fresh
release epic in m2. Each keeps its Goal and recurrence paragraph and requires
the new `/quest/m1/README.md`, and the cut also requires both reviews. Only
`/quest-delete` ends the recurrence.

## Required

- [Breaking release readiness](/quest/m1/README.md) - code, protocol and package proofs
- [Migration audit](/quest/m2/release/migrate-audit.md) - every break on the cut PR has its MIGRATE.md entry
- [API shape review](/quest/m2/release/api-review.md) - every new or changed public API was reviewed for a better shape, and approved changes landed
- [Feature review](/quest/m2/release/feature-review.md) - every major work line since the last release was judged end to end, and its follow-ups landed
- [Claim new npm names](/quest/m2/release/npm-first-publish.md) - trusted publishing can publish every new `@moq/*` package
