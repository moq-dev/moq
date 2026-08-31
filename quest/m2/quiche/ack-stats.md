# [M] Per-stream ACK stats

## Goal

The quiche backend reports each stream's delivered-versus-queued progress, so
moq-net can expose deeper per-subscriber latency and backlog when a relay uses
moq-uring. This is backend-specific depth after the Quinn QoS baseline, not a
prerequisite for it.

## Plan

- Fork-side work lands in the moq-dev/quiche repository; this quest covers the
  moq-side adoption in this repo plus the fork PRs.
- The backend-neutral trait hook and required Quinn implementation belong to
  moq-dev/web-transport (moq-dev/web-transport#368). Start from that released
  interface and preserve its semantics rather than defining a second
  quiche-only hook. The moq-net consumer is
  [moq#2733](https://github.com/moq-dev/moq/issues/2733).
- quiche tracks per-stream ack state internally but exposes none of it; the
  fork adds a public accessor for a stream's acked/delivered offset. Also
  surface `cwnd`: moq-uring already pulls `path_stats()` for rtt and delivery
  rate, and `cwnd` sits unread beside them because
  `web_transport_trait::Stats` has no slot for it.
- Delivered-offset plus send-timestamp gives per-viewer delivery latency;
  aggregation and any dashboard stay with the QoS questline. Nothing here
  ships UI.

## Required

- A moq-dev/web-transport release carries the backend-neutral per-stream delivered-vs-queued hook with a Quinn implementation (moq-dev/web-transport#368)

## Related

- [Backlog counters](/quest/m2/qos/backlog.md) - can consume this deeper quiche
  evidence when moq-uring is selected, but ships first against Quinn
- [Viewer feedback](/quest/m2/qos/viewer-feedback.md) - the receiver-side
  complement; ACK stats measure the same lag from the sender's chair
