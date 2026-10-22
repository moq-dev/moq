# [XS] Backport the WebPKI dependency fixes

## Goal

Release's TLS consumers use the WebPKI fixes already on main without
unrelated dependency upgrades or changes to the published API or wire.

## Plan

Decided 2026-10-22 (maintainer): include this in the focused patch backports.
At release 36f392f492, Cargo.lock has rustls-webpki 0.103.15 and
webpki-root-certs/webpki-roots 1.0.9. Main took 0.103.16 and 1.0.10 in
[moq-dev/moq#6169](https://github.com/moq-dev/moq/pull/6169).
The later [moq-dev/moq#6197](https://github.com/moq-dev/moq/pull/6197) names
the security updates but its actual patch only changes a getrandom edge.

Verify the upstream advisory fixes and update the affected locked packages
on release. Do not cherry-pick the broad dependency-audit patch, which also
upgrades GStreamer. Check the resulting dependency graph, advisory audit,
and affected TLS builds through the release gate. The consumers are existing
native clients and relays that validate peer certificates. No package version
bump, public API change, or MoQ wire change belongs in this backport.
