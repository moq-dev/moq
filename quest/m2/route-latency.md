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
- The endpoint's own first hop can be unknown too, such as a browser whose
  WebTransport reports no RTT. The application then reads the sum from the
  first relay alone, marked as excluding the first hop, never a guess.
- It is unverified, like the metric: a peer can misstate only what crosses
  it, and nothing acts on the value.
- The draft, the Rust and JS codecs, and where the application reads it land
  together, with `just test interop --all`.

Sized 2026-11-10 against the lite-07 draft, by a bufferbloat simulation
whose script and tables are in
[moq-dev/moq#8458](https://github.com/moq-dev/moq/pull/8458):

- Wire: `Down Only (8)` becomes `Flags (8)`, bit 0 down-only and bit 1
  latency present, followed by `Latency (i)` in milliseconds, PROBE's RTT
  unit, only when bit 1 is set. Unknown costs 0 bytes, a sum up to 127 ms 1
  byte, up to 16.4 s 2 bytes: ROUTE_UPDATE grows from 6-9 bytes to 7-11,
  ROUTE_START from 14-16 to 15-18.
- Filter: a node reads its link's smoothed RTT once a second and reports the
  minimum of the last 30 reads, re-advertising only when that leaves
  max(5 ms, a quarter) of what it last advertised. Under bufferbloat, reading
  every ACK sent 292 updates per link-hour, an EWMA of 1 Hz reads 37, a 20 s
  median 1.3, and the 30 s minimum 0. A rise shows after 30 s, a drop at
  once, and a standing queue shorter than 30 s sends nothing.
- Updates: a relay adds its upstream link's value to the sum it received,
  so one real shift sends one ROUTE_UPDATE per origin on each stream
  downstream of the link: R for the first link of a chain of R relays, 1 for
  the last, measured at 1.0 to 1.4 per downstream relay for R = 1 to 8.
  Jitter alone sent 0 per origin-hour up to R = 2, and 2.2 on average (9 at
  most) at R = 8.
- Cost: the fan-out of a Cost change today, every route selected over the
  link, but triggered by measurement: a link carrying 10,000 origins sends
  about 80 KB per downstream stream per shift.
- Accuracy: each hop is within max(5 ms, 25%) of its link's RTT floor, so
  the sum is within the sum of those bounds.
- An endpoint whose transport reports no RTT can still learn its first hop
  from PROBE, whose replies carry the relay's smoothed RTT; only one without
  a Probe Stream reads the sum as excluding the first hop.

## Waiting

- Maintainer: decide whether lite-07's ROUTE carries the latency sum. An agent cannot take it, since it adds a field to a wire about to freeze. Options: the sum as sized above, costing 0 to 2 bytes on ROUTE_START and ROUTE_UPDATE and one ROUTE_UPDATE per origin per downstream stream for each real shift, none for jitter; or no wire change, leaving applications to measure with a track of their own. Recommendation: the field, since the sizing meets the bar of at most one ROUTE_UPDATE per origin per shift; check that the choice is recorded in this Plan.

## Related

- [Routes and announces](/quest/m2/cluster-routing/routes/README.md) - the ROUTE messages this would extend
- [Link quality](/quest/m5/link-quality.md) - measured link cost, which feeds selection where this never does
