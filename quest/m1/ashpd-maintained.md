# [S] ashpd uses maintained D-Bus crates

## Goal

A stable ashpd release uses zbus 6, so the portal capture client no longer
pulls the retired zbus_names, zvariant, or zvariant_derive crates. Upgrade
ashpd and remove RUSTSEC-2026-0370, RUSTSEC-2026-0371, and RUSTSEC-2026-0372
from `deny.toml`; `just rs audit` passes without those exceptions.

## Plan

Unblocked 2026-11-06 (/quest-unblock): [ashpd 0.14.0](https://crates.io/crates/ashpd/0.14.0),
published 2026-11-05, depends on zbus `^6.0.0`, carrying the
[zbus 6 port](https://github.com/bilelmoussaoui/ashpd/pull/491). Upgrade to it,
adapt the portal capture client to any 0.14 API changes, drop the three
exceptions, then run `just rs audit` and, on Linux,
`cargo check --locked -p moq-video --no-default-features --features pipewire`
so the portal capture client compiles against the new API.
Confirmed 2026-11-12 (/quest-audit): `cargo info ashpd` lists 0.14.0 on
crates.io; the lock still holds 0.13.13.

## Related

- [Cost audit](/quest/m3/audit-cost.md) - recheck the rest of the dependency graph
