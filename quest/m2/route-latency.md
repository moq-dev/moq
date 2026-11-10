# [S] Route latency in js/net

## Goal

`js/net` reads a lite-07 route's Min RTT as Rust does: it decodes and encodes
ROUTE_START's and ROUTE_UPDATE's Flags and optional Min RTT, a browser
publishes its own route at 0, and its announced routes carry the sum the
relay advertised plus its own first hop, marked as excluding the first hop
when the browser's transport reports no RTT. The sum never ranks or selects
a route.

## Plan

Decided 2026-11-10 (maintainer, on
[moq-dev/moq#8458](https://github.com/moq-dev/moq/pull/8458)): ✅ lite-07's
ROUTE carries the latency sum, as min RTT. Each node adds its link's windowed
minimum, the lowest of the last 30 once-a-second reads of the smoothed RTT,
re-advertised only past max(5 ms, 25%), never QUIC's lifetime min_rtt, which
never rises after a path change. The endpoint adds its own first hop from its
connection. Rejected: no wire change, leaving applications a track of their
own.

[moq-dev/moq#8458](https://github.com/moq-dev/moq/pull/8458) landed the
draft, the Rust codec and route layer, `origin::RouteInfo::min_rtt`, the
bindings' `MinRtt`, and the relay's `--cluster-withhold-min-rtt`. Its PR body
keeps the sizing simulation: one real shift sends one ROUTE_UPDATE per origin
on each stream downstream of the link, and jitter sends none.

What remains is the JS side, which needs the JS route layer first:

- Decode Flags (0x1 down-only, 0x2 Min RTT present, any other bit a
  PROTOCOL_VIOLATION) and the Min RTT that follows, in milliseconds, and
  encode a browser's own route with a Min RTT of 0.
- Add `MinRtt` (`upstream`, `link`, `total()`) to `js/net`'s `RouteInfo`,
  named as Rust's. `link` follows the same windowed minimum over
  WebTransport `getStats()`'s smoothed RTT, and is `undefined` where it
  reports none.
- Pin the codec to Rust's with the shared corpus, and run
  `just test interop --all`.

## Required

- [js/net route layer](/quest/m2/cluster-routing/routes/js.md) - the ROUTE codec and route-backed `RouteInfo` this extends

## Related

- [Routes and announces](/quest/m2/cluster-routing/routes/README.md) - the ROUTE messages this extends
- [Link quality](/quest/m5/link-quality.md) - measured link cost, which feeds selection where this never does
