# [M] Route latency

## Goal

The maintainer has decided whether a lite-07 ROUTE carries an informational
latency sum, and the choice is carried out before the cut. If it does, each
node adds its link's smoothed RTT to the sum on the route it selected before
advertising it, so an endpoint learns the latency to an origin node through
any number of relays by adding its own first hop, with no extra track and no
knowledge of the relays between. The sum never ranks or selects a route.

## Plan

Planned 2026-11-09 (maintainer, /quest-plan from Kixel's audit of its fleet
plan): ✅ settle lite-07's route-layer additions before the cut, since a
field on ROUTE after it needs another wire version.

Why it is worth deciding: an application that wants to show the round trip
between two peers through a cluster has no way to today without publishing a
track for it. A per-hop message keyed by the announce hop list was proposed,
but lite-07 removed that list for one distance-vector route per origin, and
such a route cannot name its path, so a sum is all the wire can carry. The
[status track](/quest/m3/mesh/status.md) publishes per-link RTT for mesh
nodes only, so it cannot answer for a path through an operator's relays.
[moq-dev/moq#3769](https://github.com/moq-dev/moq/pull/3769) priced links by
measurement and was closed for "measurement first, with operator-owned
routing policy"; this is the measurement alone.

If the field is taken, it holds to these:

- `route_order` and feasibility never read it. The metric stays the
  operator's static cost, and [link quality](/quest/m5/link-quality.md)
  stays the opt-in way to price a link by measurement.
- It comes from what QUIC already measures on the link, quantized, with
  hysteresis, so jitter sends no ROUTE_UPDATE
  ([RFC 9616](https://www.rfc-editor.org/rfc/rfc9616.html) is the reference
  for RTT that feeds a routing protocol). Report the ROUTE_UPDATEs one real
  shift causes across a chain of relays.
- A node may withhold it, and a sum that crosses such a node is unknown from
  there on, marked without a sentinel value. An operator decides whether its
  relays report.
- It is unverified, like the metric: a peer can misstate only what crosses
  it, and nothing acts on the value.
- The draft, the Rust and JS codecs, and where the application reads it land
  together, with `just test interop --all`.

Size the field against the lite-07 draft first, then bring the maintainer the
choice below with that size and the update count.

## Waiting

- Maintainer: decide whether lite-07's ROUTE carries the latency sum. An agent cannot take it, since it adds a field to a wire about to freeze. Options: a quantized, informational sum on ROUTE_START and ROUTE_UPDATE as above, or no wire change, leaving applications to measure with a track of their own. Recommendation: the field, if the sizing shows a shift costs a ROUTE_UPDATE per origin at most; check that the choice is recorded in this Plan.

## Related

- [Routes and announces](/quest/m2/cluster-routing/routes/README.md) - the ROUTE messages this would extend
- [Link quality](/quest/m5/link-quality.md) - measured link cost, which feeds selection where this never does
