# [XL] DPDK

## Goal

A relay build whose io_uring workers are replaced by DPDK poll-mode workers
owning a NIC queue each, measured against the kernel path on the same host.
Worth building only on hardware with SR-IOV virtual functions or a dedicated
NIC; neither Linode nor OVH VPS, where the fleet runs, offers either.

## Plan

Do not start until the [AF_XDP spike](/quest/m2/af-xdp.md) has a verdict: it
runs on today's hosts and bounds what bypass can buy. If it shows the kernel
path is within reach of line rate, this quest is abandoned.

When a provider offers the hardware: a `moq-dpdk` worker crate mirroring
`moq-uring`'s worker contract (one QUIC endpoint per worker, connection-ID
steering by the NIC's RSS or a flow rule), userspace UDP/IP, and the noq
endpoint driven from the poll loop. Measure packets per second, CPU per
Gbps, and p99 latency against io_uring on the same box, then decide whether
a bare-metal tier is worth operating.

## Required

- A moq.pro relay provider offers SR-IOV or bare-metal hosts the fleet can
  run on

## Related

- [AF_XDP UDP path](/quest/m2/af-xdp.md) - the no-hardware verdict this waits
  on
