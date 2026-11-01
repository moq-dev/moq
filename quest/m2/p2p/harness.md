# [M] Harness

## Goal

A Playwright harness under `test/p2p` that measures moq-lite over the data
channel against WebTransport through the relay on the same LAN and across a
NAT, browser to browser, browser to native, and native to native over ICE
and QUIC, in Chrome and Firefox with
Safari best effort, and writes a JSON report each mapping or cost decision
quotes.

## Plan

A Playwright driver with a local relay, `moq-bench` publishing synthetic
frames of known size and rate through it, and `moq-cli --p2p` as the native
hop. The NAT rows run the peers behind a network-namespace NAT with a
local STUN server, so a reflexive pair is exercised without leaving the host.

Matrix:

- browser to native: the same `moq-cli` hop over WebTransport and over the
  qmux channel;
- browser to browser: a publishing tab to a watcher tab directly, and the
  same publisher to two watcher tabs directly, against the same pairs through
  the relay. A watcher tab never re-serves (decided 2026-10-01), so there is
  no tab-transit row;
- native to native: two `moq-cli --p2p` nodes over ICE and QUIC, on the LAN
  and across the NAT, against the same pair through the relay. There is no
  native data channel row.

Add publisher/consumer and roster/selected-pair sweeps, including explicit
full-mesh selection, roster churn, pending negotiations and cancellation.
Report connection counts, signaling bytes, wakes, CPU and retained state;
the STUN roster gate alone does not bound pairing. Verify the CDN control
session survives peer handover while unused CDN payload demand stops.
Real CDN/P2P route-health cases extend this harness from the m2
[Route health](/quest/m2/route-health/README.md) line, not here.

Metrics: sustained throughput ceiling, per-frame latency p50 and p99 from the
timestamp `moq-bench` stamps into each keyframe (same host, so one clock),
stall duration after an induced loss on the ordered channel, join time
including the WebTransport-first fallback and the relay-first subscription
that P2P then migrates, and migration gap at the switch. Record the browser
versions and the Firefox mDNS caveat beside each row.

State the boundary beside the result: loopback and one wifi segment say
nothing about multicast-filtered venues or many-tab fan-out on real access
points.

## Required

- [moq-cli joins](/quest/m2/p2p/cli.md)
- [Signaling and policy](/quest/m2/p2p/signal.md)
