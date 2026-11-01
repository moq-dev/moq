# [M] MagicDNS

## Goal

`ssh host.mesh` and `curl http://host.mesh:8080` work unmodified. The daemon
resolves `*.<mesh>` names to a per-peer loopback address and listens there on
that peer's served ports, forwarding each connection as a byte stream.

## Plan

Decided 2026-10-25 (maintainer): loopback addresses per peer, no TUN device.
Names come from enrollment: every `moq_auth::mesh::Enrollment` lists each
member's unique, DNS-safe name, refreshed with the token. Settle how the
resolver is installed per OS (split DNS through systemd-resolved, macOS
`/etc/resolver`) and how the loopback range is chosen and kept stable across
restarts, and record both here. `install` sets it up; uninstalling removes it.

## Required

- [Daemon](/quest/m2/mesh/daemon.md) - the forwarder and service list
