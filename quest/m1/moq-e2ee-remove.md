# [XS] Remove the moq-e2ee stub

## Goal

`rs/moq-e2ee` leaves the workspace, along with any release or version config
that names it, so release-plz stops tracking a crate whose final release is
out. moq-e2ee 0.0.22, the final no-code release pointing users to
moq-crypto, is on crates.io.

## Plan

Decided 2026-11-05 (maintainer, /quest-audit): ✅ retire the stub before
[Rust releases publish through OIDC](/quest/m3/release-rs-oidc.md), so the
crate never needs a trusted publisher.

Published 2026-11-05 (maintainer, /quest-unblock, by an agent at the
maintainer's request): moq-e2ee 0.0.22 from `main`'s stub, with only the
version raised from 0.0.11 locally, since 0.0.21 on crates.io (2026-11-03) and
on `release` is still the full implementation. A 0.0.x caret requirement never
resolves to the next patch, so no dependent upgrades into the stub
unasked. Not chosen: publishing it through the m2 cut; deleting the crate
with no final release.

- Delete `rs/moq-e2ee` (the workspace's `rs/*` glob picks it up) and its
  `Cargo.lock` entry.
- Search release-plz, `version.json`, workflows, and docs for `moq-e2ee` and
  drop what names the crate. The `moq-e2ee-*` draft profiles and the
  `draft-lcurley-moq-e2ee` drafts are the protocol, not the crate, and stay.

Public API: none; the crate has no code. Wire: none.
