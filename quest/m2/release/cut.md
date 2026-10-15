# [M] Cut and publish the prepared breaking release

## Goal

The prepared main branch reaches release with the finalized wire and intended
package versions, and the release-triggered publication jobs complete.

## Plan

Follow CONTRIBUTING.md's release procedure: merge release into main first,
then main into release using merge commits. Confirm the maintainer's cut
instruction before executing it. Prepare the package-specific semver changes
at that time; Rust publication remains owned by release-plz. Run candidate
compatibility, draft, package, and all-language interop checks, and verify
publication outcomes. Separate artifact install proofs and package-specific
tags follow this cut. This planning PR cuts and publishes nothing.
After publication, re-measure the planned wire-compat breaks against the
published versions, running them rather than skipping them, as
[Re-measure planned wire-compat breaks](/quest/m1/compat-remeasure.md)
adds to the release procedure.

## Required

- [Breaking release readiness](/quest/m1/README.md) - code, protocol and package proofs
