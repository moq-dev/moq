# [XS] Remove the moq-e2ee stub

## Goal

`rs/moq-e2ee` leaves the workspace, along with any release or version config
that names it, so release-plz stops tracking a crate whose final release is
out. moq-e2ee 0.0.21, the final no-code release pointing users to moq-crypto,
is on crates.io (published 2026-11-03).

## Plan

Decided 2026-11-05 (maintainer, /quest-audit): ✅ retire the stub before
[Rust releases publish through OIDC](/quest/m1/release-rs-oidc.md), so the
crate never needs a trusted publisher.

- Delete `rs/moq-e2ee` (the workspace's `rs/*` glob picks it up) and its
  `Cargo.lock` entry.
- Search release-plz, `version.json`, workflows, and docs for `moq-e2ee` and
  drop what names the crate. The `moq-e2ee-*` draft profiles and the
  `draft-lcurley-moq-e2ee` drafts are the protocol, not the crate, and stay.

Public API: none; the crate has no code. Wire: none.
