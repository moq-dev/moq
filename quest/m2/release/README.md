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
from their registries before downstream pin migrations. Existing upgrade and
identity guides describe public API, wire, archive-format, and legacy limits.

Downstream staging, mixed-version upgrade/rollback, canaries, and broad rollout
follow publication and require their own operator authorization. Do not mark
production stability proven merely because upstream artifacts published.

## Required

- [API-diff check](/quest/m2/release/migrate-audit.md) - PRs into release diff the public API; backports refuse breaks, the cut confirms MIGRATE.md coverage
- [Cut and publish](/quest/m2/release/cut.md) - independently executable after m1; release-triggered publication
- [Published catalog readers](/quest/m2/release/catalog-readers.md) - released Go and JS clients accept the current Rust publisher's live catalog in smoke; an earlier release-line patch may satisfy it
- [Published assets](/quest/m2/release/assets-release.md) - npm carries hostable worklets
- [C++ publication](/quest/m2/release/cpp-release.md) - archives and matching OBS plugin publish and install
- [Generated C publication](/quest/m2/release/c-publish.md) - released archives run the C samples and interop client
- [Dart publication](/quest/m2/release/dart-publish.md) - the wrapper and native artifacts install from the registry
- [Final libmoq stub](/quest/m2/release/libmoq-final-release.md) - publish the migration notice before removing its source
- [Retire libmoq](/quest/m2/release/libmoq-retire.md) - cleanup only after that publication
- [Final hand-written C notice](/quest/m2/release/c-final-release.md) - publish the migration notice before source retirement
- [Retire hand-written C](/quest/m2/release/c-retire.md) - complete the final old-package notice and source retirement
- [Hosted worklet adoption](/quest/m2/release/dogfood-assets.md) - downstream consumers exercise the published assets
- [moq.sh deployment](/quest/m2/release/moq-sh-deploy.md) - verify its release-triggered publication
