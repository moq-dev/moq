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

// Hidden until its first track exists, then announced.
const route = origin.route(Moq.Path.from("chat.room"), { hidden: true });
const track = route.broadcast.createTrack("messages", { timescale: Moq.Time.Timescale.MILLI });
const group = track.appendGroup();
group.writeFrame({ payload: new TextEncoder().encode("hello"), timestamp: Moq.Time.Timestamp.now() });
group.finish();
route.announce();
```

- **The origin holds the routes, not the connection.** `origin.route(path)` creates a route that owns a new broadcast; fill its tracks through `route.broadcast`. Closing a session withdraws its announcements and leaves the routes for the next one. A route is visible at once unless created `{ hidden: true }`, which keeps it invisible, locally and remotely, until `announce()`. `withdraw()` hides it again and `announce()` brings the same stream back. `close()` retracts it and closes its broadcast, as closing the broadcast does.
- **Answer claimed paths with a route.** `origin.claim(prefix)` returns a `Claim` over a prefix, and `claim.requested()` yields each requested path. Answer with `request.route()`, a new exact route under the claim's instance (a fresh one if the claim names its own), so on `moq-lite-07-wip` readers resolved through the claim continue (older wires mint another instance for it, restarting their subscription), or `request.route({ hidden: true })` to fill its broadcast before `announce()` answers the requesters; closing it or its broadcast before then refuses them as unroutable, so `using job = request.route({ hidden: true })` refuses on a throw. Serve a broadcast you already hold with `request.serve(broadcast)`, or refuse with `request.reject(err)`.
- **End content by finishing tracks.** A broadcast has no finished state. When the input ends, `finish()` each track, then `close()` it; closing a track without finishing aborts it. The broadcast stays announced and keeps serving them until you `close()` it. How long to keep it up for late readers is yours to decide.
- **Requests can pin an instance.** `consumer.request(path, { instance })` resolves only through a route announcing that [publisher instance](/concept/moq-lite#publisher-instances). The resolved broadcast's `instance` names the stream it came through, pinned or not. `origin.route(path)` mints a random instance when it is created and keeps it across `withdraw()` and `announce()`. Unless one is named, a claim moves to a fresh instance each time a job holding its current one ends, and a job takes the claim's. Pass `origin.route(path, { instance: { id, epoch } })` only for a publisher that continues its group sequences across restarts and raises the epoch each time.
- **Follow restarts.** `origin.follow(path)` follows whichever publisher instance serves the path: its `active` changes to a fresh broadcast on a restart, clears on an end, and resolves again on the next start, so read it reactively and close it when done. Instances are never stitched, so subscribe again to each new broadcast. Another route of the same instance taking over (instances travel only on `moq-lite-07-wip`; older wires mint one per announcement) keeps `active` as is, and each track subscription resumes there from the first group it lacks. A refusal ends it. Pass `{ instance }` to pin one stream, which it waits for again after an end. `request(path)` instead keeps its accepted instance and ends once that instance stops serving; while pending, it fails `Unroutable` once discovery of its path has synced without a route for it. To list what is live, iterate `announced(scope)`. See [identity and recovery](/concept/identity) to choose between following and holding one instance.
- **Discovery follows what you watch.** A `consume` origin asks the relay for announcements only under the prefixes its readers watch with `announced(scope)`, `request(path)`, or `follow(path)`, stops asking about a prefix 2s after nothing at or beneath it is watched, and asks again on reconnect. Every request and follower watches its path from creation until closed, so it outlives the listing that found it. A relay that refuses an announce request downgrades that session: requests subscribe blind through it, and followers keep waiting for a session that can discover.
- **Connections share by URL and versions.** `new Connection({ url })` pools and reconnects with backoff, which the elements use. An ordered `versions` list, spelled like `moq-lite-06` as in Rust, applies to both WebTransport and WebSocket; equivalent lists share, while a different order or an omitted list gets its own pool entry. Supplying your own transport options or origin selects a private loop. Receive estimates are on demand: observe `connection.probe` reactively or subscribe while you need them; `peek()` alone opens no PROBE stream.
- **GOAWAY moves the session.** The connection dials the replacement at once while the old session keeps serving its groups, up to `goaway.handover` (default 10s, or the relay's deadline when that is sooner). New requests keep opening on the old session until the replacement's route outranks it, which is a restart when the instance did not cross: an unpinned announced request follows the replacement, while a blind or instance-pinned request stays on its accepted stream. When `moq-lite-07-wip` is offered the instance crosses, the replacement serves the same stream, and open subscriptions move across without missing a group. An empty GOAWAY redials the same URL. A redirect stays on the same host unless `goaway.redirect` is `"follow"`, which also presents the tokens on the connection's `auth` handle to the new host. A one-shot session from `Connection.connect` never moves on its own; its `draining` settles with the GOAWAY, so you can connect again and close it. Its URI is the peer's to choose and `goaway.redirect` does not apply, so validate it before dialing it or presenting tokens to it.
- **One send estimate per connection.** `Bandwidth.Allocator` divides it by track priority, max-min fair within a tier. An idle track claims nothing, though its reservation still reads the share it would get. Publishers reserve against it so their targets sum to the estimate. A connection samples the estimate from its transport only while a reservation reads it, so receive-only and lingering connections do no stats work, and the first reservation after none reads `undefined` until one fresh snapshot lands. A transport without an estimate reads `undefined`: the WebSocket fallback, WebRTC, WebTransport without `getStats()` (shipping Chromium), and Safari, whose stub `getStats()` (a 0 RTT at setup) the connection treats as absent.
- **One receive estimate per connection.** `connection.viewers` divides it among watched broadcasts, max-min fair by each viewer's ceiling, so several players on one pooled connection don't each select against the whole downlink. Holding a viewer requests PROBE, and the viewers' ceilings, summed, are the PROBE target a relay advertising `Increase` probes the downlink up to. `setTarget(0)` opts out. `watch`, `update`, and `setTarget` throw a `RangeError` when the ceilings or the target would pass `Viewers.MAX_TARGET` (`Number.MAX_SAFE_INTEGER`), the most a PROBE target carries.
- **List a broadcast's tracks.** `for await (const event of broadcast.tracks())` reports each track the broadcast lists (`added`, with its `Track.Info`), one `synced`, then `added` and `removed` as tracks come and go, until the broadcast ends; leaving the loop stops listing. Over a session it needs `moq-lite-07-wip`, and syncs when the peer does, so a track added after `synced` is new at the source. A track served only on request is never listed. See [discovery](/concept/moq-lite#discovery).
- **One track per name.** Concurrent subscriptions share one request and producer. `createTrack` on a name with a request still queued answers it. The application owns sequence numbers: the broadcast keeps nothing of an ended track, so a track that replaces it, created or requested, starts at 0. A name always means the same content, so the replacement continues past its predecessor's groups by writing its first group with an explicit sequence. Sequences are `bigint`, so every ID the wire carries fits, past 2^53 included: `track.createGroup(5n)`, `{ absolute: 10n }`. Frame indices and relative anchors stay `number`.
- **Serve history on request.** `track.requests()` opens a handle that serves the groups readers miss: ones their ranges name below the newest group written that the cache no longer holds, or never did. Each run of misses arrives as a `Track.Fill` from `await requests.requestedFill()`, shared by every reader missing it. `accept(sequence)` it into the cache and write the group's frames as first published, `dropGroups(start, end)` what the source lacks (sequences are `bigint`), and serve `wanted()` until it is `undefined`: a reader that leaves or narrows stops wanting its groups. `close()` the fill when done; whatever is still wanted is named dropped. A `fetchGroup` of an uncached group asks the handle at once, even for a group not written yet, which a live write then answers; with no handle open it fails with `NotFound`, as in Rust, and a reader's miss is given up. A reader only asks for what its `delay.max` still wants, so a reader at the live edge never asks. This serves lite FETCH and lite-07 ranged subscriptions from the same track, as Rust's `requests()` does. A moq-transport FETCH is refused, so a moq-transport reader never reaches it.
- **Timing is per track, with no default.** A track that declares a `timescale` carries a `timestamp` on every frame; one without is [untimed](/concept/moq-lite#subscriptions) and each frame carries `Time.Timestamp.unscaled(group.sequence)`, its group's key, which has no time: `as`, `asMillis`, and `asMicros` throw on it. A scaled timestamp on an untimed track, an unscaled one on a timed track, or another key throws `TimestampMismatch`. Omitting it means untimed, as in Rust. Nothing stamps a frame for you: the publisher passes the timestamp, and the `Timed` type (`{ value, at }`) carries one into the `@moq/json` producers.
- **Durations and instants are `Clock` types.** Every duration (`maxAge`, `delay`, timeouts, RTT) is a `Clock.Duration` and every monotonic time a `Clock.Instant`, from [`@moq/clock`](./clock), which `@moq/net` re-exports as `Clock`: `maxAge: Moq.Clock.Duration.secs(30)`. Neither converts to a number implicitly; read one with an explicit unit, such as `asMillis()`. The wire still carries whole milliseconds, rounded up. Frame timestamps stay `Time.Timestamp`, a tick count at the track's timescale.
- **A subscription's `delay` is a `Track.Delay { min, max }`, as in Rust.** Staleness is `delay.max`: it counts from a group's successor, by media time and by wall clock, whichever reaches it first. `delay.min` is the target latency, carried on moq-lite 07; a peer that cannot carry it leaves it `null` (not stated). Passing the old `maxAge` or `maxDelay` key throws a `TypeError` naming `delay.max`. The publisher's own staleness bound is the separate `Track.Info.maxAge`, which caps `delay.max` and fetches.
- **Authored tracks default to a zero `maxAge`.** `createTrack` and `accept` take a `Track.InfoInit`: an omitted `maxAge` is zero, so readers get only the live edge; `null` sets no publisher limit, and a `Clock.Duration` is that bound, as `Info::default()` and `with_max_age(None)` do in Rust. A track whose readers reach back for superseded groups (history, a fetch, a range from an absolute start) sets one explicitly; media takes `@moq/hang`'s 30-second `Container.MAX_AGE`. `maxAge` is how long a superseded group stays fresh, never the newest group, so a track that writes one snapshot stays readable to a late joiner. A received `Track.Info` uses `null` for none (`maxAge`, `timescale`, `refresh`), so it serves onward unchanged.
- **The cache keeps an idle group for 30 seconds.** A track evicts a group nothing has written for 30 seconds, matching the Rust cache's default expiry; a live track's newest group is exempt until a newer one arrives, unless it failed. Neither `Track.Info.maxAge` nor `delay.max` changes that window: they only decide which cached groups a reader still receives.
- **Subscription defaults.** `Track.Subscription.default()` returns the default subscription, whose zero `delay.max` skips a superseded group at once; calling `subscribe()` with no options means the same. An omitted `delay` field means the same zero; `max: null` sets no subscriber limit.
- **A subscription asks for `ranges`.** Each [range](/concept/moq-lite#subscriptions) starts and ends at an `absolute` group, one `relative` to the latest, or the group containing a `time` (a `Timestamp`), and a reader reads only its own ranges. A read whose ranges all end finishes once every group in them is handed out, dropped, or skipped, as in Rust. A time anchor needs a timed track and `moq-lite-07-wip` over a session. Passing the old `groups` key throws a `TypeError` naming `ranges`; `withRange` only narrows reads locally.
- **Hidden paths** stay out of discovery unless the announce request opts in. See [hidden broadcasts](/concept/moq-lite#hidden-broadcasts).
- **Authorization is in band.** On moq-lite 07 (opt in with `versions: ["moq-lite-07-wip", "moq-lite-06"]`, listing every fallback you still want), `auth.grant` watches what the relay lets this side publish and subscribe to, and `auth.add(token)` presents another token without reconnecting, returning its `Auth.Credential` at once; `await credential.answered` waits for the peer's AUTH verdict on moq-lite 07, resolves at once on moq-transport, and rejects with `Unsupported` on older moq-lite. Publishing outside the grant closes the connection with `SessionCode.Unauthorized`, naming the path. On moq-lite, a subscription or track listing the grant stops covering resets with `StreamCode.Unauthorized` and the session stays up. On moq-transport, `auth.add(token)` only keeps the token for the requests that name it: a route created with the `Auth.Credential` it returns (`origin.route(path, { auth })`) carries it on that announcement's `PUBLISH_NAMESPACE`; `route.setAuth(other)` renews the request. A `Connection`'s `auth` handle (a getter on a pooled connection, following its URL) outlives its sessions: a token added there is presented on every reconnect and keeps riding the announcements that name it, with no re-add or re-announce. A one-shot session's `auth` ends with it, unless `Connection.connect` was given a `new Auth.Handle()` as `auth`.
- **A graceful close waits.** `await connection.close()` withdraws announcements and drains for up to one second. `moq-lite-05` and newer wait for the subscriber to receive finished tails; older Lite versions wait only for transport ACKs. IETF media streams are not drained. `abort()` ends immediately.

Examples:
[`js/net/examples/`](https://github.com/moq-dev/moq/tree/main/js/net/examples).
Runs in the browser and, over WebSocket, in Node, Bun, and Deno; see
[server-side](/lib/js/#server-side). Path patterns are on the
[concept page](/concept/moq-lite#path-patterns).

Groups retain a 32 MiB live tail without limiting their lifetime or frame count.
Readers that fall behind the shed front receive `Evicted`; tracks with a
`refresh` ratio also request a fresh group. A join never starts at an arbitrary
frame in the retained tail.

Frames have no size cap either. `group.createFrame({ size })` returns a
`Moq.Frame.Producer` that writes one in pieces, and `group.nextFrame()` returns a
`Moq.Frame.Consumer` that reads it as it arrives with `readChunk()`, so a
file-sized frame passes through holding only the tail. The tail sheds a large
frame's oldest 1 MiB segments while it is written, so a chunk reader more than
the tail behind gets `Evicted`. `readFrame()` still works for any size, but it
collects the frame into one buffer. A compressed track keeps a 32 MiB frame
limit, because its header declares each compressed slice's size before the
bytes:

```ts
const frame = group.createFrame({ size: file.size });
for await (const chunk of file.stream()) frame.write(chunk);
frame.finish();

const reader = await consumer.nextFrame();
for (let chunk = await reader?.readChunk(); chunk; chunk = await reader?.readChunk()) save(chunk);
```

## Sockets

Use `Moq.Socket.Endpoint` for [point-to-point sessions](/concept/socket), with the same
origin publishing and consuming through the relay. An endpoint's first dial announces its
hidden client broadcast and its first accept its hidden server broadcast, so grants decide
which roles it may take; accepting verifies that the named peer actually dialed this session.
Every hop needs moq-lite-07 or moq-transport. Lite-07 currently requires explicitly
offering `moq-lite-07-wip`; older moq-lite hops do not carry these hidden endpoints. While
the endpoint's own client connection is moq-lite before 07, its dials and accepts fail as
unsupported instead of waiting. A dialer can't tell a peer behind such a hop from one not
online yet, so its dial waits as for any absent peer.

```ts
const connection = await Moq.Connection.connect({
    url, versions: ["moq-lite-07-wip"],
    publish: origin.consume(), consume: origin,
});
const endpoint = new Moq.Socket.Endpoint(origin, Moq.Path.from("game"), "viewer");
const session = await endpoint.dialTcp("server", { signal: abort.signal });
// A socket session is scale-less, so a frame carries its key, here the first.
session.producer.writeFrame({ payload: new TextEncoder().encode("hello"), timestamp: Moq.Time.Timestamp.unscaled(0n) });
session.producer.finish(); // FIN in this direction; the peer may still reply.
for (let frame = await session.consumer.readFrame(); frame; frame = await session.consumer.readFrame()) {
    receive(frame.payload);
}
endpoint.close();
connection.close();
```

A server takes `await endpoint.acceptTcp()`, then accepts or rejects that incoming dial.
The incoming peer and session are unverified claims; `accept()` can fail if the original
dial has closed. A pending dial waits for acceptance until it is cancelled or the endpoint
or session closes, with no acceptance timeout.
TCP and track dials have independent accept queues. For independent messages, use
`dialTrack` / `acceptTrack`: the stream exposes a track producer and an already subscribed
consumer in ascending order until updated. It skips groups that never arrive whole,
including refused, dropped, or evicted groups; a cut group is never a complete message.
TCP joins its one group live, recovering the head with a joining FETCH or fill over
moq-transport. Both stream types expose `peer` and
`session`; read and write through their producer and consumer handles.

JS TCP currently reads without fastest-reader pacing. A reader that falls behind the
group's bounded tail fails with an eviction error, so keeping up is required for lossless
delivery. Finishing a producer is FIN; aborting, closing the endpoint, or releasing a TCP
reader early resets the session. Close endpoints explicitly, and use `AbortSignal` to
cancel pending dials and accepts. Keep session ids secret.
