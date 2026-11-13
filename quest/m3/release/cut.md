# [M] Cut and publish the prepared breaking release

## Goal

The prepared main branch reaches release with the finalized wire and intended
package versions, and the release-triggered publication jobs complete.

## Plan

Follow RELEASE.md's release procedure: merge release into main first,
then main into release using merge commits. Confirm the maintainer's cut
instruction before executing it. Right after merging release into main, set
every eligible package with `just version 0.22.0` for this cut, then one
minor up per later API or wire cut, following
[Versions](/RELEASE.md#versions), including packages that did not change. `@moq/qmux` and the Rust `qmux` crate are
exempt from that version (`version.json`), so check each against its
registry by hand: npm already has `@moq/qmux` 0.3.3 and 0.3.4, published
from moq-dev/web-transport, while `js/qmux` is at 0.3.3. Publishing 0.3.3
fails, and `workspace:^` would resolve `@moq/net`'s dependency to
web-transport's 0.3.4, a different API. `js/qmux` must publish above 0.3.4;
the maintainer picks the version (found 2026-11-07 by the API shape audit).
Recommended 2026-11-12 (/quest-audit): 0.4.0, so a consumer of
web-transport's `^0.3` never resolves to the new API.
Before publishing, confirm moq-dev/web-transport's release config no longer
publishes `@moq/qmux` or `qmux` (the freeze step of
[qmux-js-retire](/quest/m3/release/qmux-js-retire.md) and
[qmux-rs-retire](/quest/m3/release/qmux-rs-retire.md)): otherwise it could
publish a 0.3.x above ours, which `@moq/net`'s `^` range would pick up with
the old API. Rust
publication remains owned by release-plz. This supersedes keeping `moq-rs` at
its unpublished 0.6.0 (decided 2026-11-01). Run candidate
compatibility, draft, package, and all-language interop checks, and verify
publication outcomes. Separate artifact install proofs and package-specific
tags follow this cut. This planning PR cuts and publishes nothing.
After publication, re-measure the planned wire-compat breaks against the
published versions, as RELEASE.md's release procedure describes. Until the
[planned-break remeasure](/quest/m0/interop-green/planned-break-remeasure.md)
moves that step into the release pipeline (decided 2026-11-11), run it by hand;
the cut does not wait on that quest (decided 2026-11-12).

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

Recurs once per cut (decided 2026-10-19 as once per major release; per cut,
in three kinds, 2026-11-09). The kinds, what each publishes, and which checks
each runs are in [Releases](/RELEASE.md#releases). This run is the
`0.22.0` cut: it still waits on all of m2. Once it merges into `release` and
release-plz has published every package it bumps (the API diff's baselines), a
PR on `main` sets `phase` in `version.json` to `additive` (the cut PR keeps
the old phase, so its API diff only reports its breaks) and performs the re-split the root
[questline](/quest/README.md) describes. This epic's remaining publication and
adoption quests move to a release epic in m0, m3's other quests sort by kind
into m1 (additive), m2 (API breaks) and m3 (wire changes), m4 stays, m5 moves
to m6 and m5 starts empty, each milestone README's Goal is rewritten for its
new role, and each audit's recurrence names m4 instead of m3.

Every cut's PR removes this path from every `Required`, which releases the
audits waiting on it, rewrites every link to a moved path, and re-creates this
quest as `/quest/m0/release/cut.md`, in m0's release epic (created if the
cut moved nothing there), keeping its Goal and this paragraph, with no `Required` and a `## Waiting` item for the
maintainer's instruction naming the next cut and its kind. Each later run
follows that kind:

- **Additive**: the readiness checks, with the API diff refusing any break.
  Changed JS packages and binding wrappers take patch bumps on `main` first;
  release-plz proposes the Rust patches.
- **API or wire**: first re-create the [API shape
  review](/quest/m3/release/api-review.md), the [feature
  review](/quest/m3/release/feature-review.md), the [Cargo feature
  review](/quest/m3/release/feature-flags.md), and the [migration
  audit](/quest/m3/release/migrate-audit.md) beside it in m0's release
  epic, each keeping
  its Goal and recurrence paragraph, scoped to what changed since the last
  breaking cut and requiring nothing, and add them to this quest's
  `Required`. Then advance the shared minor with `just version`.
- **Wire**, which ends the cycle: also require `/quest/m2/README.md` and
  `/quest/m3/README.md`, which the cut can wait on because it lives in m0,
  not m3; finalize the cycle's wire version, then, once it merges
  into `release` and its packages are published, set `phase` back to `additive` on `main`, and shift the milestones: remaining publication and adoption
  quests to m0, m4's quests by kind into m1, m2 and m3, m5's to m4, and m6
  stays the deferred bucket.

The `0.22.0` cut and each wire cut also re-create the cut gate in m3 from the
template below; a recurring cut-gate child that the last run re-created in
the later-work milestone, such as the [vendor
sync](/quest/m3/vendor-sync/README.md), moves under it.
Only `/quest-delete` ends the recurrence.

From the 2026-10-25 pre-cut run:

- Write the moq-cli and moq-gst `MIGRATE.md` entries from the Impact section of
  [moq-dev/moq#6522](https://github.com/moq-dev/moq/pull/6522); that rename list exists nowhere else.

From [moq-dev/moq#7702](https://github.com/moq-dev/moq/pull/7702), which
merged moq-e2ee and moq-sign into moq-crypto (decided 2026-11-02, no yank):

- `rs/moq-e2ee` is gone from `main`, but crates.io's newest moq-e2ee is a
  full 0.0.25 crate published 2026-11-11 from `release`, not the code-free
  0.0.22 pointer (corrected 2026-11-12, /quest-audit). Don't merge a
  `release` branch release-plz proposal that bumps it again;
  [retired names](/quest/m3/release/retired-names.md) publishes the pointer
  above 0.0.25, with the other names this cut stops publishing.

Open as of 2026-11-13 (maintainer, /quest-plan): where the next cycle's cuts
live. The steps above place them in m0's release epic and have the wire cut
require m2 and m3, but no quest may require one in a later milestone, and
[m4](/quest/m4/README.md) and [m5](/quest/m5/README.md) now require this cut
until it lands, a gate each cycle should re-create. [Cut
placement](/quest/m0/cut-placement.md) settles both before this cut
re-creates its successor.

Cut gate template (moved here 2026-11-09 from the gate's own Plan; see the
decision below). The gate is an epic in m3 for the checks the cut needs that
break nothing: `main` is no slower than `release`, vendored code has the
upstream fixes it needs, and the hand-written `moq-c` is at parity with
moq-ffi. It has no work of its own and completes when every child merges. It
recurs once per release cycle, always in m3, and never re-creates itself: m3
holds it and the cycle's wire cut requires m3, so a copy made before that cut
would block it forever. Cuts earlier in the cycle do not wait on it, but each
confirms on its candidate that the gate's finished checks still hold.
Re-create its README with that Goal and this recurrence, a fresh "main is no slower than release" child with its Goal and no
measurements yet, and the [vendor sync](/quest/m3/vendor-sync/README.md)
questline that the last run re-created in the later-work milestone. Only `/quest-delete` ends the
recurrence.

Decided 2026-11-09 (maintainer, /quest-unblock): the cut, not the gate,
confirms on its candidate commit that the gate's children's results still
hold, and records that commit here. ✅ Move the recheck into the cut, and
delete the gate while it has no children (rejected: the gate requiring every
other m2 entry by hand; a /quest-plan interview). The gate had no children
left and nothing made it wait for the rest of m2, so it showed as ready, and
a candidate commit recorded then would go stale before the cut.

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

- [Cut placement](/quest/m0/cut-placement.md) - where this cut re-creates its successor, and the gate it re-creates
- [Breaking release readiness](/quest/m2/README.md) - code, protocol and package proofs
- [Migration audit](/quest/m3/release/migrate-audit.md) - every break on the cut PR has its MIGRATE.md entry
- [API shape review](/quest/m3/release/api-review.md) - every new or changed public API was reviewed for a better shape, and approved changes landed
- [Feature review](/quest/m3/release/feature-review.md) - every major work line since the last release was judged end to end, and its follow-ups landed
- [Cargo feature review](/quest/m3/release/feature-flags.md) - every Cargo feature was judged against the feature rules, and approved changes landed
- [Interop and Video pass on main](/quest/m0/interop-green/README.md) - the cut PR's release gate runs `interop.yml`, which failed on `main` every day through 2026-11-12
- [Nightly passes on main](/quest/m0/nightly/README.md) - the release gate runs `features.yml`, red in the 2026-11-12 Nightly
- [Claim new npm names](/quest/m3/release/npm-first-publish.md) - trusted publishing can publish every new `@moq/*` package
