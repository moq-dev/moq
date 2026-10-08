# [M] Link quality

## Goal

A link can take its cost from its measured quality instead of configuration,
so a drone mesh steers around a fading radio link without manual tuning,
while a small wobble never flips a route. Static configured cost stays the
default and is what the CDN runs; measured cost is opt-in per link. This is
local policy feeding the one metric on the wire, with no wire change.

## Plan

Decided 2026-10-01: the wire carries one additive metric and each node
computes its own link costs.

Decided 2026-10-08: m2, and Related rather than Required for the
[cluster routing line](/quest/m1/cluster-routing/README.md): static costs are
the default and measured cost is opt-in, so the line ships without it.

- Derive the cost from what QUIC already measures (smoothed RTT, loss); an
  ETX-style estimate (Babel RFC 8966 Appendix A, B.A.T.M.A.N.'s TQ) is the
  reference. Pick the formula from the harness, not from intuition.
- Hysteresis: only advertise a cost change past a threshold or after it
  holds, so the route layer sees a few changes per real shift. Report how
  many ROUTE changes a fading link causes.
- Never mix it into business pricing: moq.pro's
  [priced topology](https://github.com/moq-dev/moq.pro/blob/main/quest/m3/priced-topology.md)
  keeps RTT out of monetary costs, and that stays true because measured cost
  is opt-in on links the operator chooses.
- A lossy-link fixture over the mock transport with mocked time.

Public API: a per-link way to ask for measured cost in the peer entry.
Wire: none.

## Required

- [Routes and announces](/quest/m1/cluster-routing/routes.md) - a cost change costs one ROUTE per origin there, instead of a re-announce per broadcast
