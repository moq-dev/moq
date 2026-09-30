# [M] AF_XDP UDP path

## Goal

A measured verdict on an AF_XDP socket as the io_uring worker's UDP path on
the hosts the fleet runs today. virtio-net supports AF_XDP in copy mode on any
Linode or OVH instance and zero-copy where the driver allows; if it lifts the
per-worker packet ceiling or cuts CPU per Gbps materially against the
io_uring path, full kernel bypass becomes a concrete question; if not, it is
closed until hardware changes.

## Plan

- A prototype `moq-uring` UDP path over an `AF_XDP` socket with a minimal
  XDP program steering the relay's port to the worker's queue, UDP and IP
  headers built in userspace, GSO replaced by the ring's batch. Keep it
  behind a feature; nothing ships.
- Measure packets per second, CPU per Gbps, and p99 latency on one virtio
  host in copy mode, on the chat and fanout shapes, against the io_uring path
  with the zero-copy quests' best settings.

Decided in the 2026-09-30 audit: moved to m3. The relay packages don't ship
io_uring yet, so a bypass that competes with it has no deployment to
improve.

## Required

- [Relay io_uring packages](/quest/m2/relay-io-uring-package.md) - the io_uring path this is compared against ships first
