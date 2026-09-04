# [M] Verdict

## Goal

A written verdict with numbers, no threshold agreed in advance: moq-lite over
data channels against WebTransport on the same LAN, in both stream mappings,
browser to browser and browser to native, in Chrome and Firefox with Safari
best effort. A go promotes the line to m2 with a JS transit quest and the
productization work; a no-go is a written abandonment that keeps the numbers.

## Plan

Harness under `test/p2p`, shaped like the `test/wasm` Playwright driver: a
local relay, `moq-bench` publishing synthetic frames of known size and rate
through it, and `moq-cli --p2p` as the LAN hop. The page measures each path
and writes a JSON report the verdict PR quotes.

Matrix:

- browser to native: the same `moq-cli` hop over WebTransport, qmux channel,
  and per-stream channels;
- browser to browser: a publishing tab to a watcher tab directly in both
  mappings, against the same pair through the relay over WebTransport.

Metrics: sustained throughput ceiling, per-frame latency p50 and p99 from the
timestamp `moq-bench` stamps into each keyframe (same host, so one clock),
recovery after a dropped group in the per-stream mapping, join time including
the WebTransport-first fallback, and behavior as open channels approach
Chrome's cap. Record the browser versions and the Firefox mDNS caveat beside
each row.

State the boundary beside the result: loopback and one wifi segment say
nothing about multicast-filtered venues or many-tab fan-out on real access
points.

## Required

- [moq-cli joins the LAN](/quest/m3/p2p/cli.md)
- [Signaling over the relay](/quest/m3/p2p/signal.md)
