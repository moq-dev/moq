# [S] A macOS client opens extra paths

## Goal

A moq-tokio client on macOS opens a path per configured interface, as it
does on Linux, so a laptop on Wi-Fi and Ethernet or a tethered phone keeps
its session when one link drops. Until then a macOS client configured to
open an extra path refuses to start.

## Plan

Decided 2026-11-26 (`/quest-plan` via kixel-ai/kixel.ai): ✅ macOS and
Windows come last, at lower priority than Android and iOS, since nothing
with two links waits on them (rejected: Linux and macOS together in the
first work).

The Darwin interface selection lands with
[iOS](/quest/m4/quic-multipath-ios.md). What is left here is to enable it in
moq-tokio's config on macOS and test it on a host with two interfaces: the
second path validates, and taking the sending interface down leaves the
session open.

Public API: none; the Linux config starts working on macOS. Wire: none.

## Hosts

- macos - the test needs two real interfaces on a Mac

## Required

- [An iOS client opens a path per interface](/quest/m4/quic-multipath-ios.md) - the Darwin interface selection this enables

## Related

- [QUIC multipath](/quest/m3/quic-multipath/README.md) - the decisions this builds on
