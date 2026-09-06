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

const broadcast = origin.publish(Moq.Path.from("chat.room"));
const track = broadcast.createTrack("messages");
const group = track.appendGroup();
group.writeString("hello");           // or writeFrame({ payload, timestamp })
group.close();

// Subscribe
const consumer = connection.consume(Moq.Path.from("chat.room")).track("messages").subscribe({ priority: 0 });
for (;;) {
    const group = await consumer.recvGroup();
    if (!group) break;
    console.log(await group.readString());
}
```

- **Origins** hold the broadcasts, not the connection: closing a session unannounces them but leaves them published for the next one. `origin.request(path)` prefers a local publish, so a page that watches what it publishes reads its own copy with no round trip.
- **Connections** race WebTransport against WebSocket and expose a `closed` promise. `Connection.Shared` pools one connection per relay URL and reconnects with backoff, which the elements use.
- **Discovery** by prefix (`origin.announced(prefix)`), and `origin.announce(prefix, provider)` to advertise a whole subtree served on demand.
- **Subscriptions** carry a priority and max age; groups arrive out of order and are read frame by frame, with `Lagged` when frames were evicted before you read them.
- **Datagrams** on moq-lite 05+ and fetch-by-sequence for history.
- **Errors** split by scope: a stream reset throws `StreamError` with a `StreamCode`, a session close gives `SessionError` with a `SessionCode`. The registries are disjoint, so the same number means different things in each, and 64+ is yours. Same on either transport. Named conditions like `Lagged` subclass `StreamError`, so one `code` check catches a gap whether it happened here or at the peer, and resetting a moq-lite stream with one sends that code rather than a bare internal error. IETF streams use their own mapping: cancellation sends CANCELLED, other local failures send INTERNAL\_ERROR, and received codes remain opaque.
- **Paths** with `Path.relative` for the cross-broadcast catalog references hang uses.

Examples in
[`js/net/examples/`](https://github.com/moq-dev/moq/tree/main/js/net/examples).
Runs in the browser and, over WebSocket, in Node, Bun, and Deno; see
[server-side](/lib/js/#server-side).
