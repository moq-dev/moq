# [S] Rust releases publish through OIDC

## Goal

`release-rs.yml` publishes crates with crates.io trusted publishing through
release-plz's native OIDC support, and the long-lived `CARGO_REGISTRY_TOKEN`
secret is removed and revoked.

## Plan

Decided 2026-10-22 from the repository improvement audit. release-plz (0.3.172
in the dev shell) exchanges the GitHub OIDC token itself, so no separate
authentication Action is needed.

- Grant the publishing job `id-token: write` and drop `CARGO_REGISTRY_TOKEN`
  from its environment. Keep the existing release guards. A job-level
  `permissions` block replaces the workflow's `contents: write` and
  `pull-requests: write`, which release-plz needs for tags, releases, and
  PRs, so add `id-token: write` beside them or repeat all three.
- Verify on the next approved release that every crate published, then delete
  the repository secret and revoke the token on crates.io.
- This removes a reusable credential; it does not protect against a
  compromised authorized workflow.

## Required

- [crates.io trusts the workflow](/quest/m2/crates-trusted-publisher.md) - every published crate has the trusted publisher configured
