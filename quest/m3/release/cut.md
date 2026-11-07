# [M] Cut and publish the prepared breaking release

## Goal

The prepared main branch reaches release with the finalized wire and intended
package versions, and the release-triggered publication jobs complete.

## Plan

Follow CONTRIBUTING.md's release procedure: merge release into main first,
then main into release using merge commits. Confirm the maintainer's cut
instruction before executing it. Right after merging release into main, set
every eligible package with `just version 0.22.0` for this cut, then one
minor up per cut, following [Versions](/CONTRIBUTING.md#versions), including
packages that did not change. Rust
publication remains owned by release-plz. This supersedes keeping `moq-rs` at
its unpublished 0.6.0 (decided 2026-11-01). Run candidate
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

This cut ships [moq-dev/moq#7234](https://github.com/moq-dev/moq/pull/7234)'s
MKV buffering fix to release, which stays exposed until then. Backport it to
`release` separately? ✅ No: defer it to the next main → release cut
(maintainer, 2026-11-03). A blanket size cap would refuse supported inputs and
miss the decoder's captured children, as
[moq-dev/moq#7718](https://github.com/moq-dev/moq/pull/7718) measured.
It likewise ships [moq-dev/moq#7708](https://github.com/moq-dev/moq/pull/7708)'s
resumed-history fix: release (moq-net 0.3.20) loses a resumed IETF reader's
cached groups on drafts 14 to 22. ✅ No backport (maintainer, 2026-11-04;
[moq-dev/moq#7815](https://github.com/moq-dev/moq/pull/7815)), so check
both fixes reach release.

Recurs once per major release (decided 2026-10-19; shift rule replaced
2026-11-05). The PR that finishes the cut shifts the milestones, as the root
[questline](/quest/README.md) describes: this epic's remaining publication
and adoption quests move to m0, m3's other quests split by kind into m1
(additive) and m2 (breaking), m4's move to m3, m5 stays, and every link to a
moved path is rewritten. A recurring cut-gate child that its last run
re-created in m3, such as the [vendor
sync](/quest/m3/vendor-sync/README.md), moves under the re-created
gate instead. That PR also removes this path from every `Required`, which
releases the audits waiting on it. It then re-creates the [cut
gate](/quest/m2/cut-gate/README.md) in m2 as that gate's Plan describes, and
this quest, the [API shape review](/quest/m3/release/api-review.md), the
[feature review](/quest/m3/release/feature-review.md), and the [Cargo feature
review](/quest/m3/release/feature-flags.md) at the same paths under a fresh
release epic in m3. Each keeps its Goal and recurrence paragraph and requires
the new `/quest/m2/README.md`, and the cut also requires all three reviews.
Only `/quest-delete` ends the recurrence.

From the 2026-10-25 pre-cut run:

- Write the moq-cli and moq-gst `MIGRATE.md` entries from the Impact section of
  [moq-dev/moq#6522](https://github.com/moq-dev/moq/pull/6522); that rename list exists nowhere else.
- Raise each wrapper's moq-ffi version floor (for example Kotlin's
  `MOQ_FFI_RANGE`) to the release that ships the `implementation` field from
  [moq-dev/moq#6775](https://github.com/moq-dev/moq/pull/6775).

From [moq-dev/moq#7702](https://github.com/moq-dev/moq/pull/7702), which
merged moq-e2ee and moq-sign into moq-crypto (decided 2026-11-02, no yank):

- Nothing to publish for moq-e2ee: its final code-free release, 0.0.22,
  already points at moq-crypto, and `rs/moq-e2ee` is gone. Don't merge a
  `release` branch release-plz proposal that bumps it.

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

The [2026-10-24 pre-cut audit](https://github.com/moq-dev/moq/pull/6707)
closed with its 37 findings' regression tests re-run together in
[moq-dev/moq#8059](https://github.com/moq-dev/moq/pull/8059). At
[main 1ae5c5f808](https://github.com/moq-dev/moq/commit/1ae5c5f808f5fb695a42dcb451611edce81802d7),
each fix's test still exists or was replaced by a later deliberate change.
These passed: the moq-net, moq-net-tests, moq-mux, moq-rtmp, moq-tokio,
moq-uring, hang, and moq-audio suites (7,027 tests, with the fuzz regression
replays), every JS package's unit tests (5,745), the `just test harness`
browser checks (48), and `just test video` (11). Nightly's harness step fails
before it reaches its browser checks, which is how two of them stayed broken
after [moq-dev/moq#7942](https://github.com/moq-dev/moq/pull/7942) until the
re-run fixed them. Run those browser checks by hand on the cut tree, along
with the suites above.

## Required

- [Breaking release readiness](/quest/m2/README.md) - code, protocol and package proofs
- [Migration audit](/quest/m3/release/migrate-audit.md) - every break on the cut PR has its MIGRATE.md entry
- [API shape review](/quest/m3/release/api-review.md) - every new or changed public API was reviewed for a better shape, and approved changes landed
- [Feature review](/quest/m3/release/feature-review.md) - every major work line since the last release was judged end to end, and its follow-ups landed
- [Cargo feature review](/quest/m3/release/feature-flags.md) - every Cargo feature was judged against the feature rules, and approved changes landed
- [Claim new npm names](/quest/m3/release/npm-first-publish.md) - trusted publishing can publish every new `@moq/*` package
