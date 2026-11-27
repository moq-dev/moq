# [M] A Windows client opens extra paths

## Goal

A moq-tokio client on Windows opens a path per configured interface, as it
does on Linux, so a machine on Wi-Fi and Ethernet keeps its session when
one link drops. Until then a Windows client configured to open an extra
path refuses to start.

## Plan

Decided 2026-11-26 (`/quest-plan` via kixel-ai/kixel.ai): ✅ macOS and
Windows come last, at lower priority than Android and iOS, since nothing
with two links waits on them (rejected: every platform in the first work).

Recommended: `moq_sock::udp`'s Windows send path already writes an
`IN_PKTINFO` with a zero interface index. Check whether Windows routes a
send by that index; if it does not, bind a socket per path with
`IP_UNICAST_IF`, which the
[protocol quest](/quest/m3/quic-multipath/core.md)'s opaque local handle
allows.

Test on a Windows host with two interfaces: the second path validates, and
disabling the sending interface leaves the session open.

Public API: none; the Linux config starts working on Windows. Wire: none.

## Hosts

- windows - the test needs two real interfaces on Windows

## Required

- [moq-tokio opens extra paths on Linux, and relays accept them](/quest/m3/quic-multipath/paths-linux.md) - the session handle and config this enables on Windows

## Related

- [QUIC multipath](/quest/m3/quic-multipath/README.md) - the decisions this builds on
