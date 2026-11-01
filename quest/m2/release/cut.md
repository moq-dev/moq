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
review](/quest/m2/release/api-review.md), the [feature
review](/quest/m2/release/feature-review.md), and the [Cargo feature
review](/quest/m2/release/feature-flags.md) at the same paths under a fresh
release epic in m2. Each keeps its Goal and recurrence paragraph and requires
the new `/quest/m1/README.md`, and the cut also requires all three reviews. Only
`/quest-delete` ends the recurrence.

From the 2026-10-25 pre-cut run:

- Write the moq-cli and moq-gst `MIGRATE.md` entries from the Impact section of
  [moq-dev/moq#6522](https://github.com/moq-dev/moq/pull/6522); that rename list exists nowhere else.
- Raise each wrapper's moq-ffi version floor (for example Kotlin's
  `MOQ_FFI_RANGE`) to the release that ships the `implementation` field from
  [moq-dev/moq#6775](https://github.com/moq-dev/moq/pull/6775).

The [2026-10-22 release audit](https://github.com/moq-dev/moq/pull/7176)
closed with its fixes rechecked together. At source candidate
[main 487759f62a](https://github.com/moq-dev/moq/commit/487759f62a9ef39d916fe5b92df06dcff83478ce),
the focused native suites (7,697 passes), JS checks, draft checks, OBS sanitizer
suite, and all 176 configured all-language interop cells passed. At
[main 1ea4b1a2c3](https://github.com/moq-dev/moq/commit/1ea4b1a2c333527e25c8c29ee827290ed6e7d410),
`just test wire-compat` against moq-cli 0.14.10, moq-relay 0.17.10, and
`@moq/net` 0.4.7 passed every stable lite-01 through lite-06 session cell,
token, and catalog/container lane, with lite-07-wip skipped as its accepted
planned break. Dart's tests and both staged publish dry-runs passed with no
warnings. Neither main is the cut tree, and the full Linux gate was not run:
repeat these checks after the documented release-to-main merge.

## Required

- [Breaking release readiness](/quest/m1/README.md) - code, protocol and package proofs
- [Migration audit](/quest/m2/release/migrate-audit.md) - every break on the cut PR has its MIGRATE.md entry
- [API shape review](/quest/m2/release/api-review.md) - every new or changed public API was reviewed for a better shape, and approved changes landed
- [Feature review](/quest/m2/release/feature-review.md) - every major work line since the last release was judged end to end, and its follow-ups landed
- [Cargo feature review](/quest/m2/release/feature-flags.md) - every Cargo feature was judged against the feature rules, and approved changes landed
- [Claim new npm names](/quest/m2/release/npm-first-publish.md) - trusted publishing can publish every new `@moq/*` package
