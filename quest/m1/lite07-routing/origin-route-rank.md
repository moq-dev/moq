# [M] The origin ranks routes as the router does, and bridged routes keep drain and priority

## Goal

A route a lite-07 relay learns from lite-06 or moq-transport keeps its GOAWAY
drain and link priority when the relay advertises it on. A live draining
route beats a dead or debouncing one. An equally specific claim filed deeper
never overrides a direct one without being ranked against it. The router,
the origin, and the draft rank routes by one definition.

## Plan

Reproduced 2026-11-21 with scratch tests in `rs/moq-net/src/model/origin.rs`:

1. **Drain and priority are lost when bridging.** `RouteMeta` (origin.rs:996)
   has neither; `take_update` (origin.rs:1407-1428) hardcodes
   `draining: false` and `Lane::default()`; `OwnClaims::apply`
   (origin.rs:4126-4127) then fills in `i8::MAX` and false. lite-06 and
   moq-transport carry neither on the wire, but both exist locally: a GOAWAY
   sets `route.draining` (`lite/subscriber.rs:742`, `:10929`,
   `ietf/subscriber.rs:2919`), and link priority sits on
   `RouteEntry.priority`. The draft (about lines 457 and 528) says a lite-07
   claim carries them. A drain that flips only the winner emits no cursor
   event, by design (`draining_is_invisible_to_readers`), so `OwnClaims` needs
   a cursor that compares rank, not just more `RouteMeta` fields. Scratch: a
   Peer link at priority -5 whose route then drained; the route node kept
   (draining false, priority 127) and the stream saw no change. `marked` is
   not lost.
2. **A dead route beats a draining one.** `route_order` puts
   `draining || !available` in one tier (origin.rs:955), and `Node::relane`
   keeps `primary` on an unavailable slot (`route/node.rs:891-895`). Scratch:
   a draining lite-06 route at cost 5 lost to an unavailable route-layer
   route at cost 1. The router ranks unavailable below draining
   (`route/relay.rs:2920-2933`); the draft lumps them (about line 574), so fix
   its text too.
3. **A deeper filing overrides without ranking.** `RouteTable::best_route`
   (origin.rs:6335-6343) overwrites at equal specificity without
   `route_order`; `TableCursor::best` compares prefix length first; `Epochs`
   keys by the filed prefix (origin.rs:896-915), not the claim's own prefix
   (the filed prefix less `above`). Scratch: a draining epoch-1 claim filed
   at `src/a` with `above = 1` beat a healthy epoch-2 claim at `src` for
   `src/a/b`.

Recommended: one rank type shared by `route/relay.rs` and `route_order`, and
stated once in the draft. Not recommended: materializing only the best
route per claimant into the origin, as Gemini proposed. It drops the
alternates [route-health failover](/quest/m3/route-health/failover.md)
needs, and it slows a front's switch. The origin must rank regardless,
because lite-06, moq-transport, and local routes never reach the router.

Fix [route-wakes](/quest/m3/route-wakes.md)'s description of `route_order`,
which puts the primary claim ahead of the drain tier (the reverse of the
code), to match what lands.

Caused by: confirm each by diff. Likely candidates:

- Item 1: [moq-dev/moq#8023](https://github.com/moq-dev/moq/pull/8023) or
  [moq-dev/moq#8987](https://github.com/moq-dev/moq/pull/8987).
- Item 2: [moq-dev/moq#8375](https://github.com/moq-dev/moq/pull/8375).
- Item 3: [moq-dev/moq#8827](https://github.com/moq-dev/moq/pull/8827) or
  [moq-dev/moq#7489](https://github.com/moq-dev/moq/pull/7489).

Public API: none. Wire: draft ranking text only.
