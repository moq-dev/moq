---
title: "@moq/net"
description: The pub/sub layer in TypeScript
---

# @moq/net

[![npm](https://img.shields.io/npm/v/@moq/net)](https://www.npmjs.com/package/@moq/net)

The TypeScript twin of [`moq-net`](/lib/rs/moq-net): connections, origins,
broadcasts, tracks, groups, and frames, negotiating moq-lite or moq-transport
at setup. The model is [moq-lite](/concept/moq-lite). This page is the behavior
the types do not spell out.

```ts
import * as Moq from "@moq/net";

const url = new URL("https://cdn.moq.dev/anon?jwt=...");

// The origin is the routing table. The broadcast survives a reconnect.
const origin = new Moq.Origin.Producer();
const connection = await Moq.Connection.connect({
    url,
    publish: origin.consume(),
    consume: origin,
});

const broadcast = origin.createBroadcast(Moq.Path.from("chat.room"));
const track = broadcast.createTrack("messages");
const group = track.appendGroup();
group.writeString("hello");
group.close();
broadcast.announce();
```

- **The origin holds the broadcasts, not the connection.** Closing a session unannounces them and leaves them created for the next one. A broadcast is invisible, locally and remotely, until `announce()`. `dynamic(prefix, route)` claims a prefix and yields each requested path to accept or reject.
- **One connection per URL, unless you opt out.** `new Connection({ url })` pools and reconnects with backoff, which the elements use. Supplying your own transport options, discovery, or origin selects a private loop.
- **GOAWAY moves the session.** The connection dials the replacement at once while the old session keeps serving its groups, up to `goaway.handover` (default 10s, or the relay's deadline when that is sooner). An empty GOAWAY redials the same URL. A redirect stays on the same host unless `goaway.redirect` is `"follow"`.
- **One send estimate per connection.** `Bandwidth.Allocator` divides it by track priority, max-min fair within a tier. An idle track claims nothing. Publishers reserve against it so their targets sum to the estimate.
- **Subscriber staleness is `maxDelay`.** It is media time. Passing the old `maxAge` key throws a `TypeError` naming `maxDelay`. Publisher retention is the separate `Track.Info.maxAge`.
- **Hidden paths** stay out of discovery unless the announce request opts in. See [hidden broadcasts](/concept/moq-lite#hidden-broadcasts).
- **A graceful close waits.** `await connection.close()` withdraws announcements and gives finished tracks up to one second. `abort()` ends immediately.

Examples:
[`js/net/examples/`](https://github.com/moq-dev/moq/tree/main/js/net/examples).
Runs in the browser and, over WebSocket, in Node, Bun, and Deno; see
[server-side](/lib/js/#server-side). Path patterns are on the
[concept page](/concept/moq-lite#path-patterns).
