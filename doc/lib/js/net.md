---
title: "@moq/net"
description: The pub/sub layer in TypeScript
---

# @moq/net

[![npm](https://img.shields.io/npm/v/@moq/net)](https://www.npmjs.com/package/@moq/net)

The TypeScript twin of [`moq-net`](/lib/rs/moq-net): connections, origins,
broadcasts, tracks, groups, and frames, negotiating moq-lite or moq-transport
at setup.

```ts
import * as Moq from "@moq/net";

const url = new URL("https://cdn.moq.dev/anon?jwt=...");

// Publish. The origin is the routing table the connection announces and serves,
// so a broadcast survives a reconnect.
const origin = new Moq.Origin.Producer();
const connection = await Moq.Connection.connect({
    url,
    publish: origin.consume(),
    consume: origin,
});

const broadcast = origin.createBroadcast(Moq.Path.from("chat.room"));
const track = broadcast.createTrack("messages");
const group = track.appendGroup();
group.writeString("hello");           // or writeFrame({ payload, timestamp })
group.close();
broadcast.announce();

// Subscribe
const request = origin.request(Moq.Path.from("chat.room"));
let active = request.active.peek();
while (!active) {
    await request.active.changed();
    active = request.active.peek();
}
const consumer = active.track("messages").subscribe({ priority: 0 });
for (;;) {
    const group = await consumer.recvGroup();
    if (!group) break;
    console.log(await group.readString());
}
```

- **Origins** hold the broadcasts, not the connection: closing a session unannounces them but leaves them created for the next one. `origin.request(path)` resolves an announced local broadcast with no round trip, so a page that watches what it publishes reads its own copy, unless a cheaper route announces the same path. Create, populate, then `announce()` for an exact path; use `dynamic(prefix, route)` when the set of paths is not known: an exact-path subscribe before the tracks exist is refused, and nobody, local or remote, can see or reach a broadcast until it announces.
- **Connections** race WebTransport against WebSocket. `new Connection({ url })` pools one connection per relay URL and reconnects with backoff, which the elements use. Supplying WebTransport/WebSocket options, discovery, delay, `goaway`, or a caller-owned origin selects a private loop; explicit `share: true` refuses those options. On a relay's GOAWAY the connection dials the replacement at once while the old session keeps serving its groups until it closes or the `goaway.handover` cap (default 10s, lowered to the relay's deadline) passes; the handle and its origin stay the same. An empty GOAWAY redials the same URL through a fresh DNS resolve. A redirect is followed onto the same host by default (`goaway.redirect: "follow"` lets the relay pick the host, `"ignore"` never moves), and moves the pooled entry to the new URL; a malformed or refused one, including a host change under a pinned certificate, ends the connection with `Error.RefusedRedirect` instead of reconnecting. `closed` settles when the handle is released (`null` on a clean close); the failure that stopped retrying the current URL is `error`, and a new URL recovers the same handle. A connection owns one send-rate sampler and one `Bandwidth.Allocator`; publishers reserve against it so their encoder targets sum to the estimate instead of each matching it.
- **Bandwidth** (`Bandwidth.Allocator`) divides the connection's send-rate estimate by track priority, max-min fair within a tier. An idle track claims nothing. The receive side is untouched.
- **Discovery** by any pattern scope (`origin.announced(scope)`, such as `room/*/chat`; default everything). Each event's `prefix` is the covered prefix relative to the origin, `captures` reports what the scope's wildcards matched when the prefix pins them, and `kind` says whether it was announced, updated, or retracted. The consumer is an async iterable. `origin.broadcasts(scope)` is a live `Getter<ReadonlyMap<Path.Valid, Route>>` of the same covered prefixes for UIs that need the current set. A borrowed `Connection.origin` also exposes `dynamic(prefix, route)` for serving paths on demand.
- **Subscriptions** carry a priority, a `Time.Milli` max age, and optional `groups` bounds. Groups arrive out of order and are read frame by frame, with `Error.TooFarBehind` when a reader asks for a frame the group never held and `Error.GroupTooLarge` when a write exceeds the cache budget and aborts the group.
- **Track ends**: `close()` ends a track at its live edge, while `finishAt(n)` declares the exclusive end ahead of it and still accepts the groups below. A subscriber reads the end with `final()` or awaits `finished()`. A remote track ends only once every group below its end has arrived or was dropped; one reset before its header arrived is skipped after the subscription's max age on moq-lite (one second without one), or after one second on IETF.
- **Datagrams** on moq-lite 05+ and fetch-by-sequence for history.
- **Errors** live under one namespace: a stream reset throws `Error.Stream` with a `StreamCode`, while a session close gives `Error.Session` with a `SessionCode`. The registries are disjoint, so the same number means different things in each, and 64+ is yours. Named conditions such as `Error.TooFarBehind`, `Error.FrameTooLarge`, and `Error.GroupTooLarge` subclass `Error.Stream`, so one `code` check handles a condition raised here or reported by the peer. IETF streams use their own mapping: cancellation sends CANCELLED, other local failures send INTERNAL\_ERROR, and received codes remain opaque.
- **Paths** with `Path.relative` for the cross-broadcast catalog references hang uses. Path patterns (`Path.Pattern`, `Path.Patterns`) are re-exported from [`@moq/pattern`](https://www.npmjs.com/package/@moq/pattern). Literal `Path` stays a coordinate.

The [path pattern](/concept/moq-lite#path-patterns) grammar lives on the
concept page.

## Patterns

`Path.Pattern` describes a set of paths; `Path.Patterns` is a union reduced
by containment. `contains` is the authorization check. `overlaps` asks
whether they share any path. `rooted` places a pattern under a literal root;
`rebase` is the inverse, and can return several residuals. `intersect` returns
the exact overlap as a union, and `captures` reports what one pattern's
wildcards stand for in a contained pattern.

```ts
import * as Moq from "@moq/net";

const scope = Moq.Path.Pattern.parse("room/**");
scope.matches("room/alice"); // true
scope.contains(Moq.Path.Pattern.parse("room/camera-*")); // true
scope.overlaps(Moq.Path.Pattern.parse("*/alice")); // true
scope.intersect(Moq.Path.Pattern.parse("*/alice")).toJSON(); // ["room/alice"]
scope.captures(Moq.Path.Pattern.parse("room/alice"))?.map((capture) => capture.text); // ["alice"]
scope.rebase("room").toJSON(); // ["**"]
Moq.Path.Pattern.parse("camera-*").rooted("room").text; // "room/camera-*"
```

## Advertising

Three operations, on an origin:

- `origin.createBroadcast(path)` returns a producer. The broadcast is
  invisible and unreachable, for local consumers and peers alike, until
  `broadcast.announce()`.
- `broadcast.announce(route)` / `broadcast.unannounce()` own that
  advertisement. Announcing again re-prices the standing route.
- `origin.dynamic(prefix, route)` claims `prefix` and every path beneath it
  (`""` claims everything). Hold the returned `Origin.Dynamic` while the
  claim should stay advertised; `close()` retracts it. A request beneath it
  that no announced local broadcast wins is an `Origin.Request` to `accept` or `reject`;
  reject what you will not serve rather than narrowing the claim, since a
  route is always a prefix on every wire.

A route is a capability, not an inventory. `origin.announced(scope)` yields
`Announce.Update` values: `prefix` is the covered prefix relative to the origin,
`captures` is one pattern per scope wildcard when the prefix pins a complete
match (otherwise `undefined`), `kind` is `"announced"`, `"updated"` (a
reprice in place), or `"retracted"`, and `route` carries hops and cost (on a
retraction, its last values). The consumer is an async iterable. A prefix is
not a broadcast name; the scope filters locally while sessions request its
literal head on the wire. Paths with a `.`-prefixed segment below that head
are [hidden](/concept/moq-lite#hidden-broadcasts) unless `announced(scope, { hidden: true })` opts in;
`broadcasts(scope, { hidden: true })` takes the same option.

Examples in
[`js/net/examples/`](https://github.com/moq-dev/moq/tree/main/js/net/examples).
Runs in the browser and, over WebSocket, in Node, Bun, and Deno; see
[server-side](/lib/js/#server-side).
