---
title: "@moq/net"
description: The pub/sub layer in TypeScript
---

# @moq/net

[![npm](https://img.shields.io/npm/v/@moq/net)](https://www.npmjs.com/package/@moq/net)
[![API](https://img.shields.io/badge/API-reference-blue)](/lib/js/api/net)

The TypeScript twin of [`moq-net`](/lib/rs/moq-net): connections, origins,
broadcasts, tracks, groups, and frames, negotiating moq-lite or moq-transport
at setup. The model is [moq-lite](/concept/moq-lite). This page is the behavior
the types do not spell out.

```ts
import * as Moq from "@moq/net";

const url = new URL("https://cdn.moq.dev/room");

// The origin is the routing table. The broadcast survives a reconnect.
const origin = new Moq.Origin.Producer();
const connection = await Moq.Connection.connect({
    url,
    // The first token rides SETUP (and `?jwt=` for older versions); others widen the grant.
    // A reloading `new Moq.Connection({ tokens })` also takes a Signal, set to replace them.
    tokens: [token],
    publish: origin.consume(),
    consume: origin,
});

const publication = origin.createBroadcast(Moq.Path.from("chat.room"));
const broadcast = publication.broadcast;
const track = broadcast.createTrack("messages", { timescale: Moq.Time.Timescale.MILLI });
const group = track.appendGroup();
group.writeFrame({ payload: new TextEncoder().encode("hello"), timestamp: Moq.Time.Timestamp.now() });
group.finish();
publication.announce();
```

- **The origin holds the broadcasts, not the connection.** Closing a session unannounces them and leaves them created for the next one. A publication controls announcements; its `broadcast` fills tracks. The broadcast is invisible, locally and remotely, until the publication calls `announce()`.
- **Answer claimed paths with `using`.** `requests(prefix)` claims a prefix and yields each requested path. Use `using prepared = request.prepare(epoch)`, fill its broadcast, then `prepared.publish()` to answer with a new publisher instance and keep the `Publication` it returns. Leaving the scope without publishing, including on a throw, refuses the request as unroutable. Explicit `prepared.close()` or closing its unpublished broadcast does the same; disposal after publishing leaves the returned publication and its fill broadcast live. Reject before preparing with `request.reject(err)`. A retained, unclosed prepared answer keeps the request pending; JavaScript garbage collection provides no prompt cleanup guarantee.
- **End content by finishing tracks.** A broadcast has no finished state. When the input ends, `finish()` each track, then `close()` it; closing a track without finishing aborts it. The broadcast stays announced and keeps serving them until you `close()` it. How long to keep it up for late readers is yours to decide.
- **Requests can pin an epoch.** `consumer.request(path, { epoch })` resolves only through a route announcing that [publisher epoch](/concept/moq-lite#publisher-epochs). The resolved broadcast's `epoch` names the route it came through, pinned or not.
- **Follow restarts.** `origin.follow(path)` follows whichever publisher instance serves the path: its `active` changes to a fresh broadcast on a restart, clears on an end, and resolves again on the next start, so read it reactively and close it when done. Instances are never stitched, so subscribe again to each new broadcast. Another route of the same epoch taking over (epochs travel only on `moq-lite-07-wip`) keeps `active` as is, and each track subscription resumes there from the first group it lacks. A refusal ends it. Pass `{ epoch }` to pin one instance, which it waits for again after an end. `request(path)` instead keeps its accepted instance and ends once that instance stops serving; while pending, it fails `Unroutable` once discovery of its path has synced without a route for it. To list what is live, iterate `announced(scope)`. See [identity and recovery](/concept/identity) to choose between following and holding one instance.
- **Discovery follows what you watch.** A `consume` origin asks the relay for announcements only under the prefixes its readers watch with `announced(scope)`, `request(path)`, or `follow(path)`, stops asking about a prefix 2s after nothing at or beneath it is watched, and asks again on reconnect. Every request and follower watches its path from creation until closed, so it outlives the listing that found it. A relay that refuses an announce request downgrades that session: requests subscribe blind through it, and followers keep waiting for a session that can discover.
- **Connections share by URL and versions.** `new Connection({ url })` pools and reconnects with backoff, which the elements use. An ordered `versions` list, spelled like `moq-lite-06` as in Rust, applies to both WebTransport and WebSocket; equivalent lists share, while a different order or an omitted list gets its own pool entry. Supplying your own transport options or origin selects a private loop. Receive estimates are on demand: observe `connection.probe` reactively or subscribe while you need them; `peek()` alone opens no PROBE stream.
- **GOAWAY moves the session.** The connection dials the replacement at once while the old session keeps serving its groups, up to `goaway.handover` (default 10s, or the relay's deadline when that is sooner). New requests keep opening on the old session until the replacement's route outranks it, which is a restart without an epoch: an unpinned announced request follows the replacement, while a blind or epoch-pinned request stays on its accepted instance. With an epoch, which travels only when `moq-lite-07-wip` is offered, the replacement serves the same broadcast, so open subscriptions move across without missing a group. An empty GOAWAY redials the same URL. A redirect stays on the same host unless `goaway.redirect` is `"follow"`, which also presents the tokens on the connection's `auth` handle to the new host.
- **One send estimate per connection.** `Bandwidth.Allocator` divides it by track priority, max-min fair within a tier. An idle track claims nothing, though its reservation still reads the share it would get. Publishers reserve against it so their targets sum to the estimate.
- **List a broadcast's tracks.** `for await (const event of broadcast.tracks())` reports each track the broadcast lists (`added`, with its `Track.Info`), one `synced`, then `added` and `removed` as tracks come and go, until the broadcast ends; leaving the loop stops listing. Over a session it needs `moq-lite-07-wip`, syncs at once with what this side holds, and the peer's tracks follow as `added`. A track served only on request is never listed. See [discovery](/concept/moq-lite#discovery).
- **One track per name.** Concurrent subscriptions share one request and producer. `createTrack` on a name with a request still queued answers it. The application owns sequence numbers: the broadcast keeps nothing of an ended track, so a track that replaces it, created or requested, starts at 0. A name always means the same content, so the replacement continues past its predecessor's groups by writing its first group with an explicit sequence.
- **Timing is per track, with no default.** A track that declares a `timescale` carries a `timestamp` on every frame; one without is [untimed](/concept/moq-lite#subscriptions) and its frames carry none. Omitting it means untimed, as in Rust. Nothing stamps a frame for you: the publisher passes the timestamp, and the `Timed` type (`{ value, at }`) carries one into the `@moq/json` producers.
- **Subscriber staleness is `maxDelay`.** It counts from a group's successor, by media time and by wall clock, whichever reaches it first. Passing the old `maxAge` key throws a `TypeError` naming `maxDelay`. Publisher retention is the separate `Track.Info.maxAge`.
- **Subscription defaults.** `Track.Subscription.default()` returns the default subscription, whose zero `maxDelay` skips a superseded group at once; calling `subscribe()` with no options means the same. Spread it to change a field (`{ ...Track.Subscription.default(), priority: 2 }`) rather than passing a bare options object, whose omitted `maxDelay` sets no subscriber limit.
- **Hidden paths** stay out of discovery unless the announce request opts in. See [hidden broadcasts](/concept/moq-lite#hidden-broadcasts).
- **Authorization is in band.** On moq-lite 07 (opt in with `versions: ["moq-lite-07-wip", "moq-lite-06"]`, listing every fallback you still want) and on moq-transport draft-18+ with the [MoQ Auth extension](/draft/moq-auth), `auth.grant` watches what the relay lets this side publish and subscribe to, and `auth.add(token)` presents another token without reconnecting. Publishing outside the grant closes the connection with `SessionCode.Unauthorized`, naming the path. On moq-lite, a subscription or track listing the grant stops covering resets with `StreamCode.Unauthorized` and the session stays up. On moq-transport, `auth.add(token)` keeps the token even without the extension, and a broadcast announced with the `Auth.Credential` it returns (`publication.announce({ auth })`) carries it on that announcement's `PUBLISH_NAMESPACE`; announcing again with another renews the request. A `Connection`'s `auth` handle (a getter on a pooled connection, following its URL) outlives its sessions: a token added there is presented on every reconnect and keeps riding the announcements that name it, with no re-add or re-announce. A one-shot session's `auth` ends with it, unless `Connection.connect` was given a `new Auth.Handle()` as `auth`.
- **A graceful close waits.** `await connection.close()` withdraws announcements and drains for up to one second. Application delivery requires explicit `moq-lite-07-wip`; older Lite versions wait only for transport ACKs. IETF media streams are not drained. `abort()` ends immediately.

Examples:
[`js/net/examples/`](https://github.com/moq-dev/moq/tree/main/js/net/examples).
Runs in the browser and, over WebSocket, in Node, Bun, and Deno; see
[server-side](/lib/js/#server-side). Path patterns are on the
[concept page](/concept/moq-lite#path-patterns).
