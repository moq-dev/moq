# Publish and adopt the breaking release

## Goal

Once m1 is complete, publish the prepared Rust crates, JavaScript packages,
and language bindings from release, verify installed artifacts, and complete
publication-dependent cleanup and downstream adoption.

## Plan

Decided 2026-10-13: readiness, publication, adoption, and rollout are distinct
stages. This epic is not a prerequisite of m1. Follow CONTRIBUTING.md: merge
release into main before the breaking cut, preserving published versions and
release-only fixes, then merge main into release without squashing. Version
bumps and tags happen at this cut, not in the milestone-planning PR.

Run the release's compatibility, package, draft, and all-language interop
checks on the candidate. Publish finalized lite-07 with first-party clients
preferring it and falling back to supported older versions. Verify artifacts
from their registries before downstream pin migrations. Package `MIGRATE.md`
files describe public API breaks; the upgrade and identity guides describe
wire, archive-format, and legacy limits.

Downstream staging, mixed-version upgrade/rollback, canaries, and broad rollout
follow publication and require their own operator authorization. Do not mark
production stability proven merely because upstream artifacts published.

## Required

- [Migration audit](/quest/m2/release/migrate-audit.md) - the cut writes a MIGRATE.md entry for every break, stating the net published-to-final change
- [API shape review](/quest/m2/release/api-review.md) - every new or changed public API reviewed for a better shape before the cut
- [Feature review](/quest/m2/release/feature-review.md) - every major work line since the last release judged end to end before the cut
- [Cargo feature review](/quest/m2/release/feature-flags.md) - defaults stay lean and every remaining feature is scoped to the crate that owns it, before the cut
- [Claim new npm names](/quest/m2/release/npm-first-publish.md) - a maintainer claims `@moq/clock`, `libav`, and `video` on npm and JSR with trusted publishing, before the cut publishes them
- [Cut and publish](/quest/m2/release/cut.md) - independently executable after m1; release-triggered publication
- [Published assets](/quest/m2/release/assets-release.md) - npm carries hostable worklets
- [C++ publication](/quest/m2/release/cpp-release.md) - archives and matching OBS plugin publish and install
- [Generated C publication](/quest/m2/release/c-publish.md) - released archives run the C samples and interop client
- [Dart publication](/quest/m2/release/dart-publish.md) - the wrapper and native artifacts install from the registry
- [Final libmoq stub](/quest/m2/release/libmoq-final-release.md) - publish the migration notice before removing its source
- [Retire libmoq](/quest/m2/release/libmoq-retire.md) - cleanup only after that publication
- [Retire hand-written C](/quest/m2/release/c-retire.md) - delete the hand-written crate once generated C publishes
- [Retire the web-transport JS qmux](/quest/m2/release/qmux-js-retire.md) - once `@moq/qmux` publishes from `js/qmux`, deprecate `@moq/web-socket-stream` and stop the second publisher
- [moq.sh deployment](/quest/m2/release/moq-sh-deploy.md) - verify its release-triggered publication
- [Runner client bump](/quest/m2/release/runner-client-bump.md) - the upstream interop runner's Rust client moves to the published moq-tokio, so it stops stamping every object
- [Smoke clients use request and follow](/quest/m2/release/smoke-bindings-rename.md) - condition: a binding release ships the renames; then the native smoke clients move to them
- [Relay loader adoption](/quest/m2/release/relay-loader-adoption.md) - once their pins carry it, embedding consumers load relay config through `Config::merge` and refuse obsolete spellings
- [Release provenance](/quest/m2/release/provenance.md) - attached binary archives carry build provenance attestations
