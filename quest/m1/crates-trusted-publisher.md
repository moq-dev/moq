# [XS] crates.io trusts the Rust release workflow

## Goal

Condition: every crate that `release-rs.yml` publishes has a crates.io trusted
publisher for `moq-dev/moq`, workflow `release-rs.yml` (and its environment,
if the job uses one). This is a maintainer action in each crate's crates.io
settings and blocks
[Rust releases publish through OIDC](/quest/m1/release-rs-oidc.md).

Check: for each crate that release-plz publishes, its crates.io settings page
lists that configuration. Delete this quest once every crate has one.

## Plan

Moved from m2 to m1 on 2026-10-29 (maintainer): after
[moq-dev/moq#7166](https://github.com/moq-dev/moq/pull/7166) moved publish
secrets into the `publish` Environment, `CARGO_REGISTRY_TOKEN` is the
remaining long-lived registry token where npm and PyPI already use OIDC.

Decided 2026-10-22: the human step is split from the workflow change so it
stays visible as ready work. A crate that has never been published still needs
its first publish done by hand before it can have a trusted publisher; list any
such crate here.
