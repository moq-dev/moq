# lite-07 routing fixes from the main → release audit

## Goal

The lite-07 route layer and moq-relay's cluster setup hold up in the setups
0.22 ships for: a CDN mesh rolling from lite-06 to lite-07 with GOAWAY
handoffs, and a local cluster that dials a CDN with a scoped grant over two
uplinks and peers on its LAN. Every non-breaking bug the 2026-11-21 audit
of `main` against `release` confirmed is fixed.

## Plan

Decided 2026-11-21 (maintainer, /quest-plan of a Gemini audit of the routing
model; four agents checked each claim against the code and reproduced most
bugs with scratch tests):

- ✅ The cut waits on the high-severity fixes: the scoped peer, the route
  floors, and the origin's route rank in this epic, plus
  NodeId Start and origin proxy in m2 (rejected: the cut
  waits on every quest here, relay config polish included; nothing gates
  the cut). [lite07-finalize](/quest/m2/lite07-finalize.md) lists them.
- ✅ Warm routing stays after the cut, as decided 2026-11-20. lite-07 needs no
  hook for it: it arrives as a new wip version or a SETUP parameter, so
  Gemini's proposal to skip unknown Subscribe Stream messages is rejected,
  and the announce stream rejects unknown types instead.
- ✅ Dropped (rejected: m3 quests for each): the hardcoded 800 ms debounce
  and the route outbox limit, with no measured failure (revisit if moq.pro
  sees premature offlines or overrun cuts); and per-peer TLS identities, a
  per-peer proxy switch, and routing fields in `/sessions`, which nothing
  uses yet.
- Refuted, no quest: `marked` inside the Node ID is load-bearing (two
  complete claim sets with different export rules); the cross-grant echo
  suppression (`stale_copy`) catches real bugs, and 11 router tests fail
  without it; JS `u62()` reads full 64-bit varints despite its name; and the
  raw `/` Path Keep split is
  [already owned](/quest/m2/lite07-path-keep-parts.md).

This epic has no work of its own.

## Related

- [Route wakes](/quest/m3/route-wakes.md) - the audit's route fan-out cost
- [Federation](/quest/m3/cluster-routing/federation.md) - the audit's priority and outbound scoping gaps
- [Warmth port](/quest/m3/cache-routing/select.md) - the audit's cache-aware routing gaps
