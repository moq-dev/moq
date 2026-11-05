# [XS] Retire the moq-e2ee stub

## Goal

`rs/moq-e2ee` is gone and the `moq-e2ee` crate is no longer published. Its
last release, a code-free crate whose README points at moq-crypto, stays on
crates.io for anyone still resolving the old name.

## Plan

Decided 2026-11-03 (maintainer, /quest-plan of the follow-ups from
[moq-dev/moq#7702](https://github.com/moq-dev/moq/pull/7702)): milestone m2,
mirroring [Retire libmoq](/quest/m2/release/libmoq-retire.md).

- #7702 leaves `rs/moq-e2ee` as a code-free stub, and
  [the cut](/quest/m2/release/cut.md) publishes it at a version above
  crates.io's 0.0.20. Check with `curl -s
  https://crates.io/api/v1/crates/moq-e2ee | jq -r .crate.max_version` that
  the stub's version is the newest.
- Then delete `rs/moq-e2ee` and its workspace member, so release-plz never
  publishes it again, and grep for any remaining `moq-e2ee` or `moq_e2ee`.
  Keep the draft and profile names (`draft-lcurley-moq-e2ee`, "moq-e2ee
  epoch"), which name the format, not the crate. (Folded in from
  moq-dev/moq#7748's duplicate "Retire the moq-e2ee stub", 2026-11-04.)
- moq-sign was never published, so it needs no stub.

## Required

- [Cut and publish](/quest/m2/release/cut.md) - publishes the stub as moq-e2ee's final release
