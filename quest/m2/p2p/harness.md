# [M] Harness

## Goal

A Playwright harness under `test/p2p` that measures moq-lite over the data
channel against WebTransport through the relay on the same LAN and across a
NAT, browser to browser and browser to native, in Chrome and Firefox with
Safari best effort, and writes a JSON report each mapping or cost decision
quotes.

## Plan

Shaped like the `test/wasm` driver: a local relay, `moq-bench` publishing
synthetic frames of known size and rate through it, and `moq-cli --p2p` as the
native hop. The NAT rows run the peers behind a network-namespace NAT with a
local STUN server, so a reflexive pair is exercised without leaving the host.

Matrix:

- browser to native: the same `moq-cli` hop over WebTransport and over the
  qmux channel;
- browser to browser: a publishing tab to a watcher tab directly, and a
  watcher tab re-serving to a second watcher, against the same pairs through
  the relay.

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
- [Transit in the JS origin](/quest/m2/p2p/transit.md) - the watcher-to-watcher row
