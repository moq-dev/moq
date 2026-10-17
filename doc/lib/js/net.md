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
const track = broadcast.createTrack("messages", { timescale: Moq.Time.Timescale.MILLI });
const group = track.appendGroup();
group.writeFrame({ payload: new TextEncoder().encode("hello"), timestamp: Moq.Time.Timestamp.now() });
group.close();
broadcast.announce();
```

- **The origin holds the broadcasts, not the connection.** Closing a session unannounces them and leaves them created for the next one. A broadcast is invisible, locally and remotely, until `announce()`. `dynamic(prefix, route)` claims a prefix and yields each requested path to accept or reject. `accept` throws on a broadcast a session delivered, since JS apps do not proxy; copy its tracks into a broadcast you produce instead.
- **Requests can pin an epoch.** `consumer.request(path, { epoch })` resolves only through a route announcing that [publisher epoch](/concept/moq-lite#publisher-epochs) and reports `unroutable` otherwise. The resolved broadcast's `epoch` names the route it came through, pinned or not.
- **Rank several CDNs.** Connect to each with its own `consume: origin.withPreference(Origin.Preference(rank))` (higher wins; 0 is neutral and a negative rank demotes), all on one origin. Requests use the preferred link that has a route and fail over to the next; see [multiple CDNs](/concept/moq-lite#multiple-cdns).
- **Follow restarts.** `origin.request(path, { announced: true })` follows whichever publisher instance serves the path: its `active` changes to a fresh broadcast on a restart, clears on an end, and resolves again on the next start, so read it reactively and close the request when done. Instances are never stitched, so subscribe again to each new broadcast. A refusal ends the request. Add `epoch` to pin one instance; a plain `request(path)` keeps its accepted instance, can resolve blindly, and ends once that instance stops serving. To list what is live, iterate `announced(scope)`.
- **Discovery follows what you watch.** A `consume` origin asks the relay for announcements only under the prefixes its readers watch with `announced(scope)` or `request(path, { announced: true })`, stops asking about a prefix 2s after nothing at or beneath it is watched, and asks again on reconnect. A plain `request(path)` asks nothing and subscribes blind, but once it resolves through a discovered route it watches its path until closed, so it outlives the listing that found it. A relay that refuses an announce request turns that session's announced requests blind too.
- **One connection per URL, unless you opt out.** `new Connection({ url })` pools and reconnects with backoff, which the elements use. Supplying your own transport options or origin selects a private loop. Receive estimates are on demand: observe `connection.probe` reactively or subscribe while you need them; `peek()` alone opens no PROBE stream.
- **GOAWAY moves the session.** The connection dials the replacement at once while the old session keeps serving its groups, up to `goaway.handover` (default 10s, or the relay's deadline when that is sooner). New requests keep opening on the old session until the replacement's route outranks it, which is a restart without an epoch: an unpinned announced request follows the replacement, while a blind or epoch-pinned request stays on its accepted instance. An empty GOAWAY redials the same URL. A redirect stays on the same host unless `goaway.redirect` is `"follow"`.
- **One send estimate per connection.** `Bandwidth.Allocator` divides it by track priority, max-min fair within a tier. An idle track claims nothing. Publishers reserve against it so their targets sum to the estimate.
- **One track per name.** Concurrent subscriptions share one request and producer. `createTrack` on a name with a request still queued answers it. The application owns sequence numbers: a track that replaces an ended one, created or requested, throws on an append that would reuse a sequence the name already wrote, so it writes its first group with an explicit sequence. Only a new broadcast starts over. `tracks()` on a broadcast producer yields each live track name, then new ones as they appear; consumers have no listing, since a relay knows only the tracks it has served.
- **Timing is per track, with no default.** A track that declares a `timescale` carries a `timestamp` on every frame; one without is [untimed](/concept/moq-lite#subscriptions) and its frames carry none. Omitting it means untimed, as in Rust. Nothing stamps a frame for you: the publisher passes the timestamp, and the `Timed` type (`{ value, at }`) carries one into the `@moq/json` and `@moq/flate` producers.
- **Subscriber staleness is `maxDelay`.** It counts from a group's successor, by media time and by wall clock, whichever reaches it first. Passing the old `maxAge` key throws a `TypeError` naming `maxDelay`. Publisher retention is the separate `Track.Info.maxAge`.
- **Hidden paths** stay out of discovery unless the announce request opts in. See [hidden broadcasts](/concept/moq-lite#hidden-broadcasts).
- **Authorization is in band.** On moq-lite 07 (offered only when `moq-lite-07-wip` is listed in `webtransport.protocols`; the WebSocket fallback cannot offer it) and on moq-transport draft-17+ with the [MoQ Auth extension](/draft/moq-auth), `auth.grant` watches what the relay lets this side publish and subscribe to, and `auth.add(token)` presents another token without reconnecting. Publishing outside the grant closes the connection with `SessionCode.Unauthorized`, naming the path. On moq-lite, a subscription the grant stops covering resets with `StreamCode.Unauthorized` and the session stays up. On moq-transport, `auth.add(token)` keeps the token even without the extension, and a broadcast announced with the `Auth.Auth` it returns (`broadcast.announce({ auth })`) carries it on that announcement's `PUBLISH_NAMESPACE`; announcing again with another renews the request. An `Auth.Auth` belongs to the session that added it, so after a reconnect, add the token again and re-announce.
- **A graceful close waits.** `await connection.close()` withdraws announcements and drains for up to one second. Application delivery requires explicit `moq-lite-07-wip`; older Lite versions wait only for transport ACKs. IETF media streams are not drained. `abort()` ends immediately.

Examples:
[`js/net/examples/`](https://github.com/moq-dev/moq/tree/main/js/net/examples).
Runs in the browser and, over WebSocket, in Node, Bun, and Deno; see
[server-side](/lib/js/#server-side). Path patterns are on the
[concept page](/concept/moq-lite#path-patterns).
