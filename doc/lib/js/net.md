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

const connection = await Moq.Connection.connect(new URL("https://cdn.moq.dev/anon?jwt=..."));

// Publish
const broadcast = new Moq.Broadcast();
connection.publish("chat.room", broadcast);
const track = broadcast.createTrack("messages");
track.appendGroup().writeFrame(new TextEncoder().encode("hello"));

// Subscribe
const announced = connection.announced("chat.");
for (;;) {
    const entry = await announced.next();
    if (!entry) break;
    const consumer = connection.consume(entry.path).subscribe("messages");
    const group = await consumer.nextGroup();
}
```

- **Connections** race WebTransport against WebSocket and expose a `closed` promise. `Connection.Reload` reconnects with backoff and re-publishes, which the elements use.
- **Discovery** by prefix, and on-demand serving when a requested track or broadcast doesn't exist yet.
- **Subscriptions** carry a priority and latency budget; groups arrive out of order and are read frame by frame, with `Lagged` when frames were evicted before you read them.
- **Datagrams** on moq-lite 05+ and fetch-by-sequence for history.
- **Remote errors** carry the peer's reset code (`RemoteError`), the same on either transport.
- **Paths** with `Path.relative` for the cross-broadcast catalog references hang uses.

Examples in
[`js/net/examples/`](https://github.com/moq-dev/moq/tree/main/js/net/examples).
Runs in the browser and, over WebSocket, in Node, Bun, and Deno; see
[server-side](/lib/js/#server-side).
