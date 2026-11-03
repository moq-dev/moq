# [S] ashpd uses maintained D-Bus crates

## Goal

A stable ashpd release uses zbus 6, so the portal capture client no longer
pulls the retired zbus_names, zvariant, or zvariant_derive crates. Upgrade
ashpd and remove RUSTSEC-2026-0370, RUSTSEC-2026-0371, and RUSTSEC-2026-0372
from `deny.toml`; `just rs audit` passes without those exceptions.

## Waiting

- bilelmoussaoui/ashpd: a stable release carries the
  [zbus 6 port](https://github.com/bilelmoussaoui/ashpd/pull/491) (merged
  2026-10-25; 0.13.13 was still the latest stable). Check crates.io.

## Plan

Until a stable port is published, wait without disabling portal capture or
adding a local fork.

## Related

- [Dependency audit](/quest/m2/audit-deps.md) - recheck the rest of the dependency graph
