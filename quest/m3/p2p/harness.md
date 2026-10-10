# [M] Harness

## Goal

A Playwright harness under `test/p2p` that measures moq-lite over the data
channel against WebTransport through the relay on the same LAN and across a
NAT, browser to browser and browser to native, in Chrome and Firefox with
Safari best effort, and writes a JSON report each mapping or cost decision
quotes.

## Plan

A Playwright driver with a local relay, `moq-bench` publishing synthetic
frames of known size and rate through it, and `moq-cli --p2p` as the native
hop. It stands alone rather than reusing `test/wasm`, which
[rs2ts](/quest/m1/rs2ts/remove-wasm.md) removes. The NAT rows run the peers behind a network-namespace NAT with a
local STUN server, so a reflexive pair is exercised without leaving the host.

Matrix:

- browser to native: the same `moq-cli` hop over WebTransport and over the
  qmux channel;
- browser to browser: a publishing tab to a watcher tab directly, and the
  same publisher to two watcher tabs directly, against the same pairs through
  the relay. A watcher tab never re-serves (decided 2026-10-01), so there is
  no tab-transit row, and native-to-native pairs use iroh, so there is no
  native data channel row either.

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

- [moq-cli joins](/quest/m3/p2p/cli.md)
- [Signaling and policy](/quest/m3/p2p/signal.md)
