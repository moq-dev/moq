---
title: MoQ Lite
description: A simple, forwards-compatible subset of MoQ Transport. Avoids some of the more complex (and dangerous) functionality.
---

# moq-lite

This website uses [moq-lite](/concept/layer/moq-lite), a subset of the IETF [moq-transport](/concept/standard/moq-transport) draft.
moq-lite is forwards compatible with moq-transport so it works with any moq-transport CDN (ex. [Cloudflare](https://moq.dev/blog/first-cdn)).
The principles behind MoQ are fantastic, but standards are **SLOW** and involve too much arguing.
My goal is to build something simple that you can use *now*, even if it's not a standard *yet*.

See the [specification](https://datatracker.ietf.org/doc/draft-lcurley-moq-lite/) for low-level details.

## API

### Terminology

- **Session** - A bidirectional connection between a client and a server.
- **Origin** - A collection of **broadcasts**, used to scope what is available to a session.
- **Broadcast** - A named and discoverable collection of **tracks** from a single publisher.
- **Track** - A series of **groups**, potentially delivered out-of-order until closed/cancelled.
- **Group** - A series of **frames** delivered in order until closed/cancelled.
- **Frame** - A chunk of bytes with an upfront size.
- **Datagram** - A single unreliable payload delivered best-effort over a QUIC datagram (lite-05+), an alternative to a group for tiny, latency-critical frames.

**NOTE:** The IETF draft uses some different names.
THE BIKE SHED MUST BE PAINTED RED.

- `Origin` -> (doesn't exist in moq-transport)
- `Broadcast` -> `Namespace`
- `Frame` -> `Object`

### Session Establishment

When a client connects to a server, it sends a list of supported ALPNs.
The server selects the first supported one to negotiate the protocol/version.

If `h3` is negotiated, then we do *another* ALPN negotiation as part of the WebTransport handshake.
It's gross but required for web browsers, so we suck it up.

Here's a list of currently supported ALPNs:

- `moql`: moq-lite, the version is negotiated via `SETUP`.
- `moq-lite-03`: moq-lite draft 3
- `moq-00`: moq-transport draft 14, the version is negotiated via `SETUP`.
- `moqt-15`: moq-transport draft 15
- `moqt-16`: moq-transport draft 16
- `moqt-17`: moq-transport draft 17
- etc...

See the Compatibility section below for more details about `moq-transport` support.

Once the QUIC or WebTransport connection is established, there is a minimal MoQ handshake.
Each endpoint sends a single `SETUP` message advertising its capabilities (for example whether it can probe the available bitrate), then you're off to the races.
The two `SETUP` messages are independent, so neither side waits for the other before getting started.
Transports that don't carry a request URI (native QUIC, or qmux over TCP/TLS) also use `SETUP` to carry the path the client wants to reach.

### Announcements

`moq-lite` optionally supports live discovery of broadcasts.

Depending on the language, there's an `announced(prefix: Path)` method on the session.
This asks the peer to notify us of any existing broadcasts that match the prefix and any future updates.

This is extremely useful for conference rooms, as you can live discover when participants join and leave.
It's also useful for individual broadcasts as you can get notifications it comes online or goes offline (no spamming F5).
The [moq-relay clustering](/bin/relay/cluster) feature actually uses this to discover other nodes in the cluster AND what broadcasts are available on each node.

The peer first replies with the set of broadcasts that are currently live, then streams updates as they change.
This initial set is a discrete batch: the latest draft reports how many entries to expect up front, so a freshly connected session can wait until that snapshot has fully arrived before listing what's available, rather than racing the gossip.

### Subscriptions

All data transfers are initiated by subscriptions.

The subscriber needs to send a `SUBSCRIBE` message indicating the **broadcast** and **track** they want (both strings).
There are additional options, such as `priority`, that primarily impact the behavior during congestion.
See the congestion section below for more details.

If the peer doesn't have the broadcast/track, they will get an error.
Otherwise, the subscription is active and will stay open until closed by the publisher (possibly with an error).

A track is broken into **groups**, each with an increasing ID.
Conceptually, these are join points, and new subscriptions will always start at the latest group.
Groups are delivered independently and potentially out of order, so you should have some logic to reorder or skip during congestion.
A group is closed when finished or aborted with an error (ex. during congestion).

A subscription starts at the latest group by default, but can name any (group, frame) position instead.
That is how a subscriber follows a track to a different publisher partway through a group, which matters when the current group may stay open indefinitely (a JSON append log, or a catalog that keeps appending deltas).
A group can therefore be assembled from more than one publisher: each contributes a disjoint run of frames, and the subscriber concatenates them in index order.

Each group consists of one or more **frames**, numbered from 0 in the order they were produced.
Frames within a group are delivered reliably and in order.
You can and should take advantage of this, for example using delta encoding.
If frames within a group are actually independent, you should probably split them into individual groups!

#### Datagrams

As an optimization (lite-05+), the publisher may deliver a small single-frame group as a **datagram** instead of opening a QUIC stream.
A datagram carries `subscribe ID | group sequence | timestamp | payload` in a single QUIC datagram, routed over the existing subscription: unreliable, unordered, never retransmitted, and capped at ~1200 bytes.
It is a separate best-effort channel parallel to groups (they share one sequence namespace), suited to tiny latency-critical frames like audio.
There is no group fallback, so a payload that doesn't fit simply isn't delivered this way.

### Congestion

If it's not obvious by now, a lot of MoQ's behavior is designed to be robust to congestion.

When congestion occurs, something **MUST** get dropped.
MoQ puts each subscriber (viewer) in control, allowing them to choose how much latency they can tolerate.
This is how the same protocol can deliver the same content anywhere between 100ms of latency to 30s of latency.

Each Subscription consists of a few properties:

- **Track Priority**: A value between 0 and 255. Tracks with higher priority will be delivered first.
- **Group Order**: The order in which groups are delivered. Defaults to descending; higher IDs are delivered first.
- **Subscriber Max Latency**: The maximum age of a non-latest group before it is skipped. Defaults to zero, so stale groups are skipped immediately.

The publisher also keeps old groups around for a best-effort **Publisher Max Latency** cache window so relays and late subscribers can still fetch them. This defaults to 5 seconds.
The subscriber's maximum latency is bounded by this window: a group can't be waited for longer than it's actually kept around.

By utilizing these properties, you can choose how your application behaves during congestion.
For example, consider a conference room with Alice and Bob:

| Track | Priority | Order | Timeout |
|-------|----------|-------|---------|
| `alice/audio` | 100 | ascending | 500ms |
| `bob/audio` | 90 | ascending | 500ms |
| `alice/video` | 50 | descending | 2s |
| `bob/video` | 40 | descending | 2s |

When combined with a local jitter buffer, this should result in different user experiences based on the network conditions:

- **No Congestion**: Every frame is delivered immediately.
- **Minor Congestion**: Bob's video might skip a few frames at the tail of each group.
- **Moderate Congestion**: Bob and Alice's video will skip the tail of each group, but audio will still be delivered.
- **Heavy Congestion**: Bob and Alice's audio might fall behind, but never more than 500ms. Video is completely dropped.

There's no optimal solution for this, but we think these subscription properties provide a GOOD ENOUGH user experience for most use-cases.
They're simple to implement and easy enough to understand.

### GOAWAY (Graceful Shutdown)

Either endpoint can gracefully drain a session by sending a `GOAWAY` message.
It tells the peer to reconnect, either to a different endpoint (a URI in the message) or to the same endpoint (empty URI), instead of being cut off when the sender shuts down.

The lifecycle on the sending side (Rust `moq-net`):

1. `session.drain()` yields the session's one GOAWAY handle, the graceful counterpart to `session.abort()`. It works on every version: one without a GOAWAY message (moq-lite-03 and earlier) simply carries no explanation to the peer.
2. `producer.send(Goaway::new())` tells the peer to reconnect to the same endpoint; `Goaway::redirect(uri)` names a different one, and `.with_timeout(duration)` adds a deadline. Sending a second is refused, so the peer never sees a URI replaced behind its back.
3. `session.closed()` resolves once the peer leaves, or once the deadline force-closes it. Only a `Goaway` carrying a timeout schedules a close of our own, so set one when the drain has to finish.

On the receiving side:

1. `session.draining()` returns a consumer: `peek()` is a cheap synchronous check, `recv()` waits for the URI and optional deadline.
2. New requests (subscribes, fetches, announce interests) on the session are then rejected; existing subscriptions keep flowing until the session closes.
3. Connect a replacement session sharing the same origin. Its announcements attach as additional routes to the broadcasts the old session serves, and when the old session closes, live subscriptions resume on the new route at a group boundary. Applications reading through `moq-net` never observe the swap.

A moq-transport client sends an empty URI: only a server can tell a peer where to reconnect. The URI is capped at 8,192 bytes on both wires, and a second GOAWAY on a session is a protocol violation that closes it.

Native clients get step 3 for free from `moq_native::Client::reconnect`, which dials the replacement while the old session keeps serving and hands over at a group boundary. `--goaway-redirect` chooses how far to trust the URI and `--goaway-handover` bounds how long the old session lingers.

`moq-relay` uses this in both directions: on shutdown it drains its own downstream sessions (see [`--drain-timeout`](/bin/relay/config#drain-timeout)), and on a GOAWAY from a cluster peer the reconnect loop migrates transparently.

GOAWAY is supported on moq-lite-04+ and IETF moq-transport draft-14+. The deadline is carried on the wire only for IETF draft-17+ (moq-lite carries no timeout, but the sender's local force-close timer still applies).

The JS `@moq/net` package decodes GOAWAY on the wire but does not yet expose this lifecycle.

## Compatibility

`moq-lite` is forward compatible with `moq-transport`.
That means for every moq-lite API, there's a corresponding moq-transport API.

That's good!
You're not locked into moq-lite and can use moq-transport in the future.
I can get hit by a bus and you wouldn't shed a tear.

When `moq-transport` wire format is negotiated, we still enforce the moq-lite API.
If the peer insists on using a moq-transport-only feature, we fake it or worst case, return an error.
For example, if there's a gap in a group (valid in moq-transport), we drop the tail of the group instead of erroring.

The following table shows the simplified compatibility matrix.
Note that there are typically 2 clients, a publisher and a subscriber.
But if a publisher needs a feature, then the subscriber needs it too, so you can lump them together.

| client        | relay         | supported | notes                                                                |
|---------------|---------------|:---------:|----------------------------------------------------------------------|
| moq-lite      | moq-lite      | ✅        |                                                                      |
| moq-lite      | moq-transport | ✅        |                                                                      |
| moq-transport | moq-lite      | ⚠️        | No moq-transport-only features.                                      |
| moq-transport | moq-transport | ⚠️        | Depends on the implementations.                                      |

### Major Differences

- **No Request IDs**: A bidirectional stream for each request to avoid HoLB. (NOTE: likely to be upstreamed into moq-transport)
- **No Push**: A subscriber must explicitly subscribe to each track.
- **Single-group FETCH only (lite-05+)**: Fetch one group by sequence, optionally bounded to a range of frames within it. Fetching across groups is not supported.
- **No Joining Fetch**: A subscription resumes by asking for the missing range directly, rather than by pairing a fetch with a subscribe.
- **No sub-groups**: SVC layers should be separate tracks.
- **No gaps**: Makes life much easier for the relay and every application.
- **No object properties**: Encode your metadata into the frame payload.
- **No pausing**: Unsubscribe if you don't want a track.
- **No binary names**: Uses UTF-8 strings instead of arrays of byte arrays.

This may seem like a lot of missing features, but in practice you don't need them.
For example, [MSF](/concept/standard/msf) doesn't use any of these features so it's fully compatible with moq-lite.
