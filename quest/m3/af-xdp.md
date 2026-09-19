# [M] AF_XDP UDP path

## Goal

A measured verdict on an AF_XDP socket as the io_uring worker's UDP path on
the hosts the fleet runs today. virtio-net supports AF_XDP in copy mode on any
Linode or OVH instance and zero-copy where the driver allows; if it lifts the
per-worker packet ceiling or cuts CPU per Gbps materially against the
io_uring path, the DPDK question becomes concrete; if not, kernel bypass is
closed until hardware changes.

## Plan

- A prototype `moq-uring` UDP path over an `AF_XDP` socket with a minimal
  XDP program steering the relay's port to the worker's queue, UDP and IP
  headers built in userspace, GSO replaced by the ring's batch. Keep it
  behind a feature; nothing ships.
- Measure packets per second, CPU per Gbps, and p99 latency on one virtio
  host in copy mode, on the chat and fanout shapes, against the io_uring path
  with the zero-copy and busy-poll quests' best settings.

## Related

- [DPDK](/quest/m4/dpdk.md) - the full bypass this verdict gates
- [#3203](/quest/m2/perf/3203-moq-uring-add-opt-in-napi-busy-polling.md) -
  the in-kernel ceiling this competes with
