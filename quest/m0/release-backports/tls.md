# [XS] Land the WebPKI release backport

## Goal

Release's TLS consumers use the WebPKI fixes already on main without
unrelated dependency upgrades or changes to the published API or wire.

## Plan

Decided 2026-10-22 (maintainer): include this in the focused patch backports.
The implementation is [moq-dev/moq#6347](https://github.com/moq-dev/moq/pull/6347),
targeting `release`. Continue there rather than starting another backport.

It updates only Cargo.lock's rustls-webpki 0.103.15 to 0.103.16 and
webpki-root-certs/webpki-roots 1.0.9 to 1.0.10, matching main. Every other
locked package and dependency edge is unchanged. No package version, public
API, or MoQ wire change is included. Existing native clients and relays
consume the corrected peer certificate validation.

The baseline advisory audit reproduced RUSTSEC-2026-0381, 0382, and 0383;
the updated audit removes all three. Dependency trees resolve the fixed
versions, including the platform-specific root-certs consumer. The full
audit still reports the four pre-existing unmaintained dependencies already
owned by the [dependency audit](/quest/m2/audit-deps.md): basic-toml,
zbus_names, zvariant, and zvariant_derive. Bans, licenses, and sources pass.

Remaining: confirm the release PR's checks and review, land it once approved,
and delete this quest and its references after the release branch contains
the fix. Publication and version bumps stay with the release owners.
