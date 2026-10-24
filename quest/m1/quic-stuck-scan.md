# [M] One stuck-window check per packet

## Goal

A moq-quic session runs its stuck-receive-window check at most once per
received packet, however many frames in that packet move connection credit.
A peer can no longer make us scan every stream for each RESET_STREAM it sends
into a shrunk window, and a benchmark shows the cost staying flat as streams
grow.

## Plan

[moq-dev/moq#5878](https://github.com/moq-dev/moq/pull/5878) added the
receive leases and the stuck check. Its last fix
already skips the check for arrivals that move no credit
(`creditless_arrivals_skip_the_stuck_check`). What remains: a RESET_STREAM
that frees credit into shrink debt still pays one scan of every stream,
which needs a local window shrink plus many streams each holding data.

Moved to m1 on 2026-10-23 (maintainer), so the cut that first publishes
moq-quic ships without this peer-driven scan.
[Lease bookkeeping](/quest/m1/quic-lease-bookkeeping.md) reworks the same
stuck-window code and eviction ranking; land it first and build on its
shape.

- Benchmark first: a RESET_STREAM storm under a shrunk window with N streams
  holding data, swept over N, to show the per-frame slope before and the
  flat cost after.
- Then mark the check dirty per frame and run it once at the end of packet
  processing. This touches the stuck-check call sites in moq-quic's tests
  (about 50) and the qmux mux path that shares it.
- Re-read the landed code first; the lease API may have moved since #5878.
  Coordinate with [qmux receive leases](/quest/m2/qmux-receive-lease.md) if
  qmux adopts leases on the same mux path.

Public API: none. Wire: none.

## Related

- [QUIC caps](/quest/m2/rate-quic.md) - paced receive credit on the same sessions
