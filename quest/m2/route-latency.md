# [S] Route latency in js/net

## Goal

`js/net` reports a lite-07 route's RTT as Rust does: its announced routes
carry the sum the relay advertised plus its own first hop, unknown when the
browser's transport reports no RTT. The sum never ranks or selects a route.

## Plan

Decided 2026-11-10 (maintainer, on
[moq-dev/moq#8458](https://github.com/moq-dev/moq/pull/8458)): ✅ lite-07's
ROUTE carries the latency sum, as RTT. Each node adds its link's windowed
minimum, the lowest of the last 30 once-a-second reads of the smoothed RTT,
re-advertised only past max(5 ms, 25%), never QUIC's lifetime min_rtt, which
never rises after a path change. The endpoint adds its own first hop from its
connection. Rejected: no wire change, leaving applications a track of their
own.

[moq-dev/moq#8458](https://github.com/moq-dev/moq/pull/8458) landed the
draft, the Rust codec and route layer, `origin::RouteInfo::rtt`, and the
bindings' `RouteInfo.rtt`, with no option to withhold it. Its PR body
keeps the sizing simulation: one real shift sends one ROUTE_UPDATE per origin
on each stream downstream of the link, and jitter sends none.

[moq-dev/moq#8443](https://github.com/moq-dev/moq/pull/8443) landed the
JS route layer with the Flags and RTT codec: a browser advertises its own
route at 0, and the subscriber decodes each route's RTT into its `Advert`
and drops it. What remains:

- Add `rtt` to `js/net`'s `RouteInfo`, named as Rust's: the advertised sum
  plus the same windowed minimum over WebTransport `getStats()`'s smoothed
  RTT, and `undefined` while either is unknown.
- Run `just test interop --all`.

## Related

- [Routes and announces](/quest/m2/cluster-routing/routes/README.md) - the ROUTE messages this extends
- [Link quality](/quest/m5/link-quality.md) - measured link cost, which feeds selection where this never does
