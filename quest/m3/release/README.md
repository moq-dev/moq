# Publish and adopt the breaking release

## Goal

Once m2 is complete, publish the prepared Rust crates, JavaScript packages,
and language bindings from release, verify installed artifacts, and complete
publication-dependent cleanup and downstream adoption.

## Plan

Decided 2026-10-13: readiness, publication, adoption, and rollout are distinct
stages. This epic is not a prerequisite of m2. Follow CONTRIBUTING.md: merge
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
