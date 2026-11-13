# [S] Relays split horizon per session

## Goal

A relay never advertises a route back over the session it learned it from.
Since [moq-dev/moq#8023](https://github.com/moq-dev/moq/pull/8023), each
neighbor holds two stream keys, so the router advertises a route back to its
source. That is loop-free, since the source refuses a route whose path names
it ([routes epic](/quest/m2/cluster-routes/README.md) Plan item 27;
by the feasibility condition until
[the switch](/quest/m2/cluster-routes/switch.md) lands), but it
doubles route traffic between every pair of relays.

## Plan

Decided 2026-11-06 (maintainer, /quest-complete, a follow-up of #8023): ✅
m3: lite-07 routing ships correct in the cut, and this trims its traffic
afterward. Not chosen: m2; m4.

Suppress the advertisement per session rather than per neighbor, and measure
route messages between two relays before and after.

Public API: none. Wire: none; behavior only.

## Related

- [A relay keeps its next hop among equal routes](/quest/m3/route-keep-next-hop.md) - the other trim of route traffic
