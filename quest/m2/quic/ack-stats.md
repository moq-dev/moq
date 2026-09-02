# [M] Expose per-stream ACK stats from the selected QUIC core

## Goal

The selected QUIC backend reports each stream's delivered-versus-queued
progress and congestion-window context, so moq-net can expose per-subscriber
latency and backlog when a relay uses `moq-uring`.

## Plan

Use the backend-neutral delivered-versus-queued hook from
moq-dev/web-transport#368. The fork adds only the protocol-core accessor and
adapter implementation needed to report a stream's acknowledged offset plus
the connection or path congestion window. Preserve the Quinn implementation's
semantics instead of creating a fork-specific stats surface.

Delivered offset plus send timestamp gives sender-side delivery latency.
Aggregation, labels, and dashboards stay in the QoS questline. Cover partial
ACKs, retransmission, reset, stream teardown, and paths whose controller does
not expose a meaningful congestion window.

## Required

- [Drive raw QUIC from the selected core in moq-uring](/quest/m2/quic/uring-raw.md) -
  provides the adapter that reports the stats
- A moq-dev/web-transport release carries the backend-neutral per-stream
  delivered-versus-queued hook with a Quinn implementation

## Related

- [Backlog counters](/quest/m2/qos/backlog.md) - consumes this deeper evidence
  when the selected uring backend is active
- [Viewer feedback](/quest/m2/qos/viewer-feedback.md) - receiver-side evidence
  for the same lag
