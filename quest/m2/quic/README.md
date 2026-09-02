# Custom QUIC: a Quinn-family fork

## Goal

Own the QUIC implementation needed by MoQ without owning a transport from
scratch. One maintained fork of Quinn or noq supplies hierarchical stream
scheduling, reliable stream resets, the shared stream state machine used by
qmux, transport telemetry, and future media experiments. The same core is
usable by the ordinary async backend and the thread-per-core `moq-uring`
backend.

Quiche remains a supported backend while parity is built, but it is no longer
the foundation for custom transport work. Removing it is not a prerequisite
for any child except the final WebTransport cutover.

## Plan

Quinn is the default parent: MoQ already ships it, its API and maintenance
surface are smaller than noq's extension-heavy fork, and the green
[qmux prototype](https://github.com/kixelated/quinn/pull/2) already targets
`quinn-proto`. Select noq instead only if its maintainers want the scheduler
and qmux work upstream and a bounded port proves that its multipath state has
not made either change materially harder. The
[parent quest](/quest/m2/quic/parent.md) records that decision before the
other fork work starts.

This replaces the three-engine backend bakeoff. Quinn and noq share the same
lineage, while the decision here turns on maintainership, patch carry, API
fit, and the ability to upstream. Performance is still a release gate: the
selected core must pass the existing relay workloads before it replaces the
quiche path.

Keep the fork as a reviewable patch stack over its parent:

- clean fixes go to the parent first;
- MoQ-specific work is designed as general QUIC or qmux primitives and offered
  upstream;
- a rejected change may remain in the fork, with the rejection and rebase cost
  recorded beside it;
- published MoQ crates never depend on a workspace-only Cargo patch or a
  mutable branch.

The scheduling contract has three levels: strict subscription priority,
byte-fair service between subscriptions at the same priority, then the
subscription's chosen group order within its own bucket. The default MoQ order
is newest group first; an ordered subscription keeps oldest first. This is a
transport API change, not a MoQ wire change.

## Quests

- [Choose the parent and establish the fork](/quest/m2/quic/parent.md) - settle
  Quinn versus noq, ownership, sync policy, and package identity
- [Land the Quinn maintenance backlog](/quest/m2/quic/quinn-maintenance.md) -
  finish the two green stream-allocation and GSO fixes already in review
- [Reliable stream reset](/quest/m2/quic/reliable-reset.md) - guarantee the
  WebTransport stream header reaches the receiver before a reset is surfaced
- [Hierarchical stream scheduling](/quest/m2/quic/scheduler.md) - strict
  subscription priority, fair buckets, and newest-first group order replace
  the lossy scalar
- [qmux on the QUIC stream state machine](/quest/m2/quic/qmux.md) - replace the
  parallel stream and flow-control implementation with the selected proto core
- [Release the fork stack](/quest/m2/quic/release.md) - publish immutable,
  consumable versions of the core and adapters
- [Drive raw QUIC from moq-uring](/quest/m2/quic/uring-raw.md) - replace quiche
  below raw moq-lite while retaining the thread-per-core runtime
- [Cut WebTransport over to the selected core](/quest/m2/quic/uring-webtransport.md) -
  reach browser parity, switch the production path, and retire the custom
  quiche fork
- [Per-stream ACK stats](/quest/m2/quic/ack-stats.md) - expose delivered versus
  queued progress and congestion-window evidence
- [Probe by early retransmission](/quest/m2/quic/probe.md) - measure capacity
  with useful retransmissions instead of padding

## Related

- [GCC egress experiment](/quest/m3/quic-gcc.md) - a measured verdict on
  WebRTC-style delay control in the selected fork
- [FEC experiment](/quest/m3/quic-fec.md) - a measured verdict on transport
  redundancy in the selected fork
- [Multipath spike](/quest/m3/multipath-spike.md) - a noq-only capability that
  informs, but does not decide, the parent choice
