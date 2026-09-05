# Custom QUIC: noq as the upstream

## Goal

Own the QUIC features MoQ needs without owning a transport. noq, the
Quinn-derived stack n0 maintains for Iroh, is the parent: MoQ-specific work
lands there as general QUIC or qmux primitives, and the same core serves the
ordinary async backend and the thread-per-core `moq-uring` backend. The
features are per-stream acknowledgment progress, reliable stream resets,
hierarchical stream scheduling, the shared stream state machine used by qmux,
and future media experiments.

Quiche remains a supported backend until noq passes the parity gate. A
moq-dev fork of noq exists only if a change MoQ needs is rejected upstream.

## Plan

noq is already the default backend on `dev`: `moq-tokio` and `moq-uring` both
compile noq-proto by default, with Quinn and quiche as explicit features. The
[parent quest](/quest/m2/quic/parent.md) turns that dependency into a working
relationship with the noq maintainers rather than a choice.

Keep every carried change reviewable against its parent:

- clean fixes go to noq first, and to Quinn when the code is shared;
- MoQ-specific work is designed as a general QUIC or qmux primitive and
  proposed upstream before any fork exists;
- a rejected change may live in a moq-dev fork, with the rejection and rebase
  cost recorded beside it;
- published MoQ crates never depend on a workspace-only Cargo patch or a
  mutable branch.

The scheduling contract has three levels: strict subscription priority,
byte-fair service between subscriptions at the same priority, then the
subscription's chosen group order within its own bucket. The default MoQ order
is newest group first; an ordered subscription keeps oldest first. This is a
transport API change, not a MoQ wire change.

## Quests

- [Establish the noq relationship](/quest/m2/quic/parent.md) - who reviews
  MoQ's proposals, how releases and advisories reach this repo, when a fork is
  warranted
- [Per-stream ACK progress in noq](/quest/m2/quic/ack-progress.md) - noq-proto
  reports how far a send stream has been acknowledged and when
- [poll_acked in web-transport](/quest/m2/quic/ack-hook.md) - the
  backend-neutral hook that awaits an acknowledged stream offset, implemented
  for noq and released
- [Land the Quinn maintenance backlog](/quest/m2/quic/quinn-maintenance.md) -
  finish the two green stream-allocation and GSO fixes already in review
- [Reliable stream reset](/quest/m2/quic/reliable-reset.md) - guarantee the
  WebTransport stream header reaches the receiver before a reset is surfaced
- [Hierarchical stream scheduling](/quest/m2/quic/scheduler.md) - strict
  subscription priority, fair buckets, and newest-first group order replace
  the lossy scalar
- [qmux on the QUIC stream state machine](/quest/m2/quic/qmux.md) - replace the
  parallel stream and flow-control implementation with noq-proto
- [Release the fork stack](/quest/m2/quic/release.md) - publish immutable,
  consumable versions of anything noq does not release itself
- [noq parity gate](/quest/m2/quic/noq-parity.md) - benchmark noq against
  quiche on the relay workloads, record the browser gaps, retire the quiche
  fork
- [Probe by early retransmission](/quest/m2/quic/probe.md) - measure capacity
  with useful retransmissions instead of padding

## Related

- [Starvation](/quest/m2/qos/starvation.md) - the first consumer of ACK
  progress: how far behind viewers are, from the relay's point of view
- [GCC egress experiment](/quest/m3/quic-gcc.md) - a measured verdict on
  WebRTC-style delay control in noq
- [FEC experiment](/quest/m3/quic-fec.md) - a measured verdict on transport
  redundancy in noq
- [Multipath spike](/quest/m3/multipath-spike.md) - a noq capability that
  MoQ does not use yet
