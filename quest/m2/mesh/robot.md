# [M] Robots

## Goal

A robot is a first-class mesh node: release builds cover aarch64 Linux, a
robot enrolls headlessly with an enrollment key, and an integration test with a
robot-shaped node (a camera broadcast plus a control stream) takes the LAN path
while its operator is local, the relay path over a flapping LTE-like uplink
otherwise, and reconnects without re-enrolling.

## Plan

Decided 2026-10-25 (maintainer): robots are a primary use case. Use the
existing network shaper for the uplink, on controlled time where the test can,
and wire it into `just test`. The control stream's latency through a flap is
reported, not gated.

## Required

- [Daemon](/quest/m2/mesh/daemon.md) - the node under test
