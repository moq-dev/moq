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
const connection = await Moq.Connection.connect(url, { publish: origin.consume() });

const broadcast = origin.createBroadcast(Moq.Path.from("chat.room"));
const track = broadcast.createTrack("messages");
const group = track.appendGroup();
group.writeString("hello");           // or writeFrame({ payload, timestamp })
group.close();
broadcast.announce();

// Subscribe
const consumer = connection.consume(Moq.Path.from("chat.room")).track("messages").subscribe({ priority: 0 });
for (;;) {
    const group = await consumer.recvGroup();
    if (!group) break;
    console.log(await group.readString());
}
```

- **Origins** hold the broadcasts, not the connection: closing a session unannounces them but leaves them created for the next one. `origin.request(path)` prefers a local broadcast, so a page that watches what it publishes reads its own copy with no round trip. Create, populate, then `announce()` for an exact path; use `dynamic(pattern, route)` when the set of paths is not known: an exact-path subscribe before the tracks exist is refused, and announcing only makes a path discoverable.
- **Connections** race WebTransport against WebSocket. `new Connection({ url })` pools one connection per relay URL and reconnects with backoff, which the elements use. `closed` settles when the handle is released (`null` on a clean close); the failure that stopped retrying the current URL is `error`, and a new URL recovers the same handle. A connection owns one send-rate sampler and one `Bandwidth.Allocator`; publishers reserve against it so their encoder targets sum to the estimate instead of each matching it.
- **Bandwidth** (`Bandwidth.Allocator`) divides the connection's send-rate estimate by track priority, max-min fair within a tier. An idle track claims nothing. The receive side is untouched.
- **Discovery** by scope (`origin.announced(scope)`, a prefix-shaped `Path.Pattern` like `live/**`; default everything). Each event's `pattern` is the claim, relative to the origin. `origin.dynamic(pattern, route)` advertises any path pattern.
- **Subscriptions** carry a priority and max age; groups arrive out of order and are read frame by frame, with `Lagged` when a reader asks for a frame the group never held and `GroupTooLarge` when a write exceeds the cache budget and aborts the group.
- **Datagrams** on moq-lite 05+ and fetch-by-sequence for history.
- **Errors** split by scope: a stream reset throws `StreamError` with a `StreamCode`, a session close gives `SessionError` with a `SessionCode`. The registries are disjoint, so the same number means different things in each, and 64+ is yours. Same on either transport. Named conditions like `Lagged` subclass `StreamError`, so one `code` check catches a gap whether it happened here or at the peer, and resetting a moq-lite stream with one sends that code rather than a bare internal error. IETF streams use their own mapping: cancellation sends CANCELLED, other local failures send INTERNAL\_ERROR, and received codes remain opaque.
- **Paths** with `Path.relative` for the cross-broadcast catalog references hang uses. Path patterns (`Path.Pattern`, `Path.Patterns`) are re-exported from [`@moq/pattern`](https://www.npmjs.com/package/@moq/pattern). Literal `Path` stays a coordinate.

The [path pattern](/concept/moq-lite#path-patterns) grammar lives on the
concept page.

## Patterns

`Path.Pattern` describes a set of paths; `Path.Patterns` is a union reduced
by containment. `contains` is the authorization check. `overlaps` asks
whether they share any path. `rooted` places a pattern under a literal root;
`rebase` is the inverse, and can return several residuals.

```ts
import * as Moq from "@moq/net";

const scope = Moq.Path.Pattern.parse("room/**");
scope.matches("room/alice"); // true
scope.contains(Moq.Path.Pattern.parse("room/camera-*")); // true
scope.overlaps(Moq.Path.Pattern.parse("*/alice")); // true
scope.rebase("room").toJSON(); // ["**"]
Moq.Path.Pattern.parse("camera-*").rooted("room").text; // "room/camera-*"
```

## Advertising

Three operations, on an origin:

- `origin.createBroadcast(path)` returns a producer. The broadcast is
  reachable by exact path immediately and invisible to discovery until
  advertised.
- `broadcast.announce(route)` / `broadcast.unannounce()` own that
  advertisement. Announcing again re-prices the standing route.
- `origin.dynamic(pattern, route)` claims every matching path. A prefix is
  `foo/**`. Hold the returned `Origin.Dynamic` while the claim should stay
  advertised; `close()` retracts it. A request under a prefix-shaped claim
  with no local broadcast is a `BroadcastRequest` to `accept` or `reject`.

A wildcard is a capability, not an inventory. `origin.announced(scope)`
yields `Announce.Update` values: `pattern` is the claim relative to the origin,
clamped to `scope`; `active` is false on a retraction, and `route` carries hops and cost while
advertised (omitted on a retraction). Use
`pattern.asPrefix()` when you need a prefix-shaped claim; an arbitrary
pattern is not a broadcast name. Resolving a non-prefix pattern into a
subscription is not implemented yet. Token grants stay prefixes.

Examples in
[`js/net/examples/`](https://github.com/moq-dev/moq/tree/main/js/net/examples).
Runs in the browser and, over WebSocket, in Node, Bun, and Deno; see
[server-side](/lib/js/#server-side).
