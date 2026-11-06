# [S] ashpd uses maintained D-Bus crates

## Goal

A stable ashpd release uses zbus 6, so the portal capture client no longer
pulls the retired zbus_names, zvariant, or zvariant_derive crates. Upgrade
ashpd and remove RUSTSEC-2026-0370, RUSTSEC-2026-0371, and RUSTSEC-2026-0372
from `deny.toml`; `just rs audit` passes without those exceptions.

## Plan

ashpd 0.14.0 (2026-11-05) depends on zbus `^6` (checked 2026-11-06,
/quest-unblock), so the upgrade can start. Don't disable portal capture or add a
local fork.

## Related

- [Dependency audit](/quest/m3/audit-deps.md) - recheck the rest of the dependency graph
