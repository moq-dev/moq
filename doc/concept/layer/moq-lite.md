---
title: MoQ Lite
description: A fraction of the calories with none of the fat.
---

# moq-lite
[moq-lite](https://www.ietf.org/archive/id/draft-lcurley-moq-lite-02.html) is a subset of the [MoqTransport](/concept/standard/moq-transport) specification.
The goal is to keep the core transport layer simple and focused on practical use-cases.

There's too much fringe functionality in the MoqTransport draft that's not practical to implement.
Most of it is specific to Cisco's implementation and bizarre requirements, so it probably won't impact you.

## API

### Terminology
- **Session** - A bidirectional connection between a client and a server.
- **Origin** - A collection of **broadcasts**, used to scope what is available to a session.
- **Broadcast** - A named and discoverable collection of **tracks** from a single publisher.
- **Track** - A series of **groups**, potentially delivered out-of-order until closed/cancelled.
- **Group** - A series of **frames** delivered in order until closed/cancelled.
- **Frame** - A chunk of bytes with an upfront size.

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
- `moqt-00`: moq-transport draft <15, the version is negotiated via `SETUP`.
- `moqt-15`: moq-transport draft 15
- `moqt-16`: moq-transport draft 16
- etc...

See the Compatibility section below for more details about `moq-transport` support.

Once the QUIC or WebTransport connection is established, there is a minimal MoQ handshake.
The `SETUP` message is primarily used to negotiate extensions, then you're off to the races!

### Announcements
`moq-lite` optionally supports live discovery of broadcasts.

Depending on the language, there's an `announced(prefix: Path)` method on the session.
This asks the peer to notify us of any existing broadcasts that match the prefix and any future updates.

This is extremely useful for conference rooms, as you can live discover when participants join and leave.
It's also useful for individual broadcasts as you can get notifications it comes online or goes offline (no spamming F5).
The [moq-relay clustering](/app/relay/cluster) feature actually uses this to discover other nodes in the cluster AND what broadcasts are available on each node.

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

Each group consists of one or more **frames**.
Frames within a group are delivered reliably and in order.
You can and should take advantage of this, for example using delta encoding.
If frames within a group are actually independent, you should probably split them into individual groups!

### Congestion
If it's not obvious by now, a lot of MoQ's behavior is designed to be robust to congestion.

When congestion occurs, something **MUST** get dropped.
MoQ puts each subscriber (viewer) in control, allowing them to choose how much latency they can tolerate.
This is how the same protocol can deliver the same content anywhere between 100ms of latency to 30s of latency.

Each Subscription consists of a few properties:
- **Track Priority**: A value between 0 and 255. Tracks with higher priority will be delivered first.
- **Group Order**: The order in which groups are delivered. Defaults to descending; higher IDs are delivered first.
- **Group Timeout**: The maximum duration to keep old groups in cache/transit. Defaults to 30 seconds.

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
- **No FETCH**: Use HTTP for VOD instead of reinventing the wheel.
- **No Joining Fetch**: Subscriptions start at the latest group, not the latest frame.
- **No sub-groups**: SVC layers should be separate tracks.
- **No gaps**: Makes life much easier for the relay and every application.
- **No object properties**: Encode your metadata into the frame payload.
- **No pausing**: Unsubscribe if you don't want a track.
- **No binary names**: Uses UTF-8 strings instead of arrays of byte arrays.
- **No datagrams**: Maybe one day.

This may seem like a lot of missing features, but in practice you don't need them.
For example, [MSF](/concept/standard/msf) doesn't use any of these features so it's fully compatible with moq-lite.
