---
title: "Media over QUIC - Lite"
abbrev: "moql"
category: info

docname: draft-lcurley-moq-lite-latest
submissiontype: IETF  # also: "independent", "editorial", "IAB", or "IRTF"
number:
date:
v: 3
area: wit
workgroup: moq

author:
 -
    fullname: Luke Curley
    email: kixelated@gmail.com

normative:
  moqt: I-D.ietf-moq-transport
  qmux: I-D.ietf-quic-qmux
  qmuxws:
    title: "QMux over WebSocket"
    target: https://datatracker.ietf.org/doc/draft-lcurley-qmux-websocket/
    author:
      -
        ins: L. Curley
        name: Luke Curley
    date: false
  RFC3986:
  RFC6455:
  RFC9002:

informative:

--- abstract

moq-lite is designed to fanout live content 1->N across the internet.
It leverages QUIC to prioritize important content, avoiding head-of-line blocking while respecting encoding dependencies.
While primarily designed for media, the transport is payload agnostic and can be proxied by relays/CDNs without knowledge of codecs, containers, or encryption keys.

--- middle

# Conventions and Definitions
{::boilerplate bcp14-tagged}


# Rationale
This draft is based on MoqTransport [moqt].
The concepts, motivations, and terminology are very similar and when in doubt, refer to existing MoqTransport literature.
A few things have been renamed (ex. object -> frame) to better align with media terminology.

I absolutely believe in the motivation and potential of Media over QUIC.
The layering is phenomenal and addresses many of the problems with current live media protocols.
I fully support the goals of the working group and the IETF process.

But it's been difficult to design such an experimental protocol via committee.
MoqTransport has become too complicated.

There are too many messages, optional modes, and half-baked features.
Too many hypotheses, too many potential use-cases, too many diametrically opposed opinions.
This is expected (and even desired) as compromise gives birth to a standard.

But I believe the standardization process is hindering practical experimentation.
The ideas behind MoQ can be proven now before being cemented as an RFC.
We should spend more time building an *actual* application and less time arguing about a hypothetical one.

moq-lite is the bare minimum needed for a real-time application aiming to replace WebRTC.
Every feature from MoqTransport that is not necessary (or has not been implemented yet) has been removed for simplicity.
This includes many great ideas (ex. group order) that may be added as they are needed.
This draft is the current state, not the end state.


# Concepts
moq-lite consists of:

- **Session**: An established QUIC connection between a client and server.
- **Broadcast**: A collection of Tracks from a single publisher.
- **Track**: A series of Groups, each of which can be delivered and decoded *out-of-order*.
- **Group**: A series of Frames, each of which must be delivered and decoded *in-order*.
- **Frame**: A sized payload of bytes within a Group.

The application determines how to split data into broadcast, tracks, groups, and frames.
The moq-lite layer provides fanout, prioritization, and caching even for latency sensitive applications.

## Session
A Session consists of a connection between a client and a server.
There is currently no P2P support within QUIC so it's out of scope for moq-lite.

The moq-lite version identifier is `moq-lite-xx` where `xx` is the two-digit draft version.
For bare QUIC, this is negotiated as an ALPN token during the QUIC handshake.
For WebTransport over HTTP/3, the QUIC ALPN remains `h3` and the moq-lite version is advertised via the `WT-Available-Protocols` and `WT-Protocol` CONNECT headers.

When UDP is unavailable, moq-lite MAY also run over reliable byte-stream transports via Qmux [qmux]; see [Transports](#transports) for the specific bindings.

The session is active immediately after the connection is established.
Both endpoints SHOULD begin sending and receiving streams right away to avoid an extra round-trip.

Optional capabilities and extensions are negotiated via a SETUP message (see [SETUP](#setup)).
Each endpoint MUST open a unidirectional Setup Stream at the start of the session, send a single SETUP message advertising what it supports, and immediately close the stream (FIN); an endpoint with no optional capabilities sends a SETUP with an empty parameter list.
Neither endpoint waits for the peer's SETUP before opening other streams.
An endpoint MUST buffer a stream whose behavior or encoding depends on a negotiated extension until the peer's SETUP arrives; everything else proceeds immediately.
As a fallback, an endpoint that opens an extension stream the peer does not support simply sees that stream reset (see [STREAM_TYPE](#stream_type)).
A negotiated capability applies only to this hop; each session is negotiated independently and relays MUST NOT forward SETUP.

While moq-lite is a point-to-point protocol, it's intended to work end-to-end via relays.
Each client establishes a session with a CDN edge server, ideally the closest one.
Any broadcasts and subscriptions are transparently proxied by the CDN behind the scenes.

## Broadcast
A Broadcast is a collection of Tracks from a single publisher.
This corresponds to a MoqTransport's "track namespace".

A publisher may produce multiple broadcasts, each of which is advertised via an ANNOUNCE_START message.
The subscriber uses the ANNOUNCE_REQUEST message to discover available broadcasts.
These announcements are live and can change over time, allowing for dynamic origin discovery.

A broadcast consists of any number of Tracks.
The contents, relationships, and encoding of tracks are determined by the application.

## Track
A Track is a series of Groups identified by a unique name within a Broadcast.

A track consists of a single active Group at any moment, called the "latest group".
When a new Group is started, the previous Group is closed and may be dropped for any reason.
The duration before an incomplete group is dropped is determined by the application and the publisher/subscriber's latency target.

Every subscription is scoped to a single Track.
A subscription starts at a configurable Group (defaulting to the latest) and continues until a configurable end Group or until either the publisher or subscriber cancels the subscription.

The subscriber and publisher both indicate their delivery preference:
- `Priority` indicates if Track A should be transmitted instead of Track B.
- `Ordered` indicates if the Groups within a Track should be transmitted in order.
- `Subscriber Max Latency` indicates the maximum age before a non-latest Group is dropped from live delivery; `Publisher Max Latency` indicates the maximum age before a non-latest Group is dropped from the publisher's cache.

The combination of these preferences enables the most important content to arrive during network degradation while still respecting encoding dependencies.

## Group
A Group is an ordered stream of Frames within a Track.

Each group consists of an append-only list of Frames.
A Group is normally served by a dedicated QUIC stream which is closed on completion, reset by the publisher, or cancelled by the subscriber.
This ensures that all Frames within a Group arrive reliably and in order.

In contrast, Groups may arrive out of order due to network congestion and prioritization.
The application SHOULD process or buffer groups out of order to avoid blocking on flow control.

A small Group MAY instead be transmitted as a single QUIC datagram when reliability is not required (see [Datagrams](#datagrams)).

## Frame
A Frame is a payload of bytes within a Group.

A frame is used to represent a chunk of data with an upfront size.
The contents are opaque to the moq-lite layer.

Each frame carries a presentation timestamp expressed in the parent Track's `Timescale` (see [TRACK_INFO](#track-info)), used by the moq-lite layer for [expiration](#expiration) decisions.

# Flow
This section outlines the flow of messages within a moq-lite session.
See the Messages section for the specific encoding.

## Connection
moq-lite runs on top of any transport that provides ordered, multiplexed, bidirectional streams: bare QUIC, WebTransport over HTTP/3 (required for web support), or Qmux [qmux] when UDP is unavailable.
See [Transports](#transports) for the bindings.

How the underlying connection is authenticated is out-of-scope for this draft.

## Transports {#transports}
moq-lite defines four transport bindings.
All four carry the same control and data streams defined elsewhere in this document; they differ only in how QUIC streams are multiplexed onto the underlying connection.

|----|---------------------|------------------|----------------------|
|    | Transport           | ALPN / Identifier | Record framing      |
|---:|:--------------------|:------------------|:--------------------|
| 1  | QUIC                | `moq-lite-xx`     | Native QUIC streams |
| 2  | WebTransport / H3   | `moq-lite-xx` (CONNECT header) | Native WebTransport streams |
| 3  | Qmux over TCP/TLS   | `moq-lite-xx` (ALPN over TLS)  | Qmux Record [qmux]  |
| 4  | Qmux over WebSocket | `moq-lite-xx` (Sec-WebSocket-Protocol) | WebSocket message [qmuxws] |

For bindings 1 and 2, moq-lite uses the underlying QUIC/WebTransport stream APIs directly.
QUIC datagrams (see [Datagrams](#datagrams)) are supported by bindings 1 and 2 only; a publisher MUST NOT emit datagrams on bindings 3 and 4.

For binding 3, a client opens a TCP connection, performs a TLS handshake, and negotiates the moq-lite version as the ALPN token.
Each direction of the TLS byte stream then carries Qmux Records as defined in [qmux].

For binding 4, a client opens a WebSocket connection [RFC6455] offering the moq-lite version as the subprotocol, per [qmuxws]: each WebSocket binary message carries one Qmux Record's `Frames` payload, with the message boundary replacing the Record `Size` field.

All other moq-lite semantics (stream types, message encoding, flow control, etc.) are identical across bindings.

## Termination
QUIC bidirectional streams have an independent send and receive direction.
Rather than deal with half-open states, moq-lite combines both sides.
If an endpoint closes the send direction of a stream, the peer MUST also close their send direction.

moq-lite contains many long-lived transactions, such as subscriptions and announcements.
These are terminated when the underlying QUIC stream is terminated.

To terminate a stream, an endpoint may:
- close the send direction (STREAM with FIN) to gracefully terminate (all messages are flushed).
- reset the send direction (RESET_STREAM) to immediately terminate.

After resetting the send direction, an endpoint MAY close the recv direction (STOP_SENDING).
However, it is ultimately the other peer's responsibility to close their send direction.

## Handshake
See the [Session](#session) section for ALPN negotiation and session activation details.

# Streams
moq-lite uses a bidirectional stream for each transaction.
If the stream is closed, potentially with an error, the transaction is terminated.

## Bidirectional Streams
Bidirectional streams are used for control streams.
There's a 1-byte STREAM_TYPE at the beginning of each stream.

|---------|--------------|-------------|
|     ID  | Stream       | Creator     |
|--------:|:-------------|:------------|
|    0x1  | Announce     | Subscriber  |
| ------- | ------------ | ----------- |
|    0x2  | Subscribe    | Subscriber  |
| ------- | ------------- | ---------- |
|    0x3  | Fetch        | Subscriber  |
| ------- | ------------- | ---------- |
|    0x4  | Probe        | Subscriber  |
| ------- | ------------- | ----------- |
|    0x5  | Goaway       | Either      |
| ------- | ------------- | ----------- |
|    0x6  | Track        | Subscriber  |
| ------- | ------------- | ----------- |

### Announce
A subscriber can open an Announce Stream to discover broadcasts matching a prefix.

The subscriber creates the stream with an ANNOUNCE_REQUEST message.
The publisher replies with a single ANNOUNCE_OK message followed by announcements for any matching broadcasts and any future changes:

- ANNOUNCE_START: a matching broadcast is available.
- ANNOUNCE_END: a previously started broadcast is no longer available.
- ANNOUNCE_RESTART: a previously started broadcast was atomically replaced.

ANNOUNCE_OK carries metadata that applies to every announcement on the stream: the publisher's own `Hop ID` (the implicit trailing entry of every announcement's path) and the number of initial announcements, which lets the subscriber deliver the initial set as a batch (see [ANNOUNCE_OK](#announce-ok)).

Each ANNOUNCE_START implicitly assigns the next Announce ID on the stream: a counter starting at 0 that increments by 1 per ANNOUNCE_START.
The id never appears on the wire; both endpoints derive it from the message order on the (reliable, ordered) stream.
ANNOUNCE_END and ANNOUNCE_RESTART reference the Announce ID instead of repeating the broadcast path.

Each broadcast has at most one current advertisement per stream.
A second ANNOUNCE_START for an already-available path is a protocol violation; an ANNOUNCE_RESTART atomically replaces the current advertisement (equivalent to ANNOUNCE_END+ANNOUNCE_START) while keeping its id live.

The subscriber MUST reset the stream if it receives an ANNOUNCE_END or ANNOUNCE_RESTART referencing an Announce ID that was never assigned or already retired, an ANNOUNCE_START for a path that is already available, or any announcement before ANNOUNCE_OK.
When the stream is closed, the subscriber MUST assume that all broadcasts are now unavailable.

Path prefix matching and equality is done on a byte-by-byte basis.
There MAY be multiple Announce Streams, potentially containing overlapping prefixes, that get their own ANNOUNCE_OK + announcements.

#### Routing {#routing}
Each announcement carries the path of Hop IDs it traversed and an accumulated Route Cost (see [ANNOUNCE_START](#announce-start)), which relays use to build a loop-free mesh.
The first entry of the reconstructed path identifies the original publisher.

A receiver MUST discard an announcement whose reconstructed path contains its own Hop ID: it has looped back, so forwarding it would extend the loop and subscribing through it would route the receiver back to itself.
This is the only loop defense moq-lite requires, and it catches loops of any length.
A Hop ID of 0 means unknown and never matches anything; withholding an ID trades loop detection for privacy.

A publisher SHOULD advertise, per stream, the best path for each broadcast whose entries avoid the origin the subscriber declared in its SETUP (see [Origin Parameter](#origin-parameter)), and nothing when every known path contains it.
Selection is per subscriber, so a subscriber that the serving path flows through still receives the best standby path, which is what lets it fail over if its own copy dies.
The per-subscriber winner changing travels as an ANNOUNCE_RESTART; the last qualifying path appearing or disappearing travels as an ANNOUNCE_START or ANNOUNCE_END.

When serving a subscription, a publisher MUST select the source by that same exclusion; if only excluded sources remain, the subscription is unroutable.
Applying one rule to both advertisement and dispatch keeps advertised paths truthful, which is what prevents subscription cycles of any length.

A subscriber that sees the same broadcast advertised across multiple streams SHOULD route subscriptions to the lowest Route Cost after adding each arriving link's cost (see [Cost Parameter](#cost-parameter)), breaking ties toward the shortest path and then toward the most recently received, so a reconnecting publisher is not outranked by the stale session it replaced.

Advertisements whose reconstructed paths share the same non-zero first entry carry interchangeable content: a relay MAY hold them as redundant routes for one broadcast and splice a live subscription across them at a Group boundary, e.g. when the serving route ends.
Cooperating redundant publishers MAY share a Hop ID to opt into this.
If the first entries differ, or either is 0 (no identity, so nothing is proven shared), they are distinct broadcasts colliding on one path: a relay MUST NOT splice between them, and SHOULD treat the later as replacing the earlier.

### Subscribe
A subscriber opens Subscribe Streams to request a Track.

The subscriber MUST start a Subscribe Stream with a SUBSCRIBE message followed by any number of SUBSCRIBE_UPDATE messages.
The publisher replies with a SUBSCRIBE_OK message once the start group is resolved, followed by any number of SUBSCRIBE_END and SUBSCRIBE_DROP messages.
For a live track the publisher MAY withhold SUBSCRIBE_OK until the first matching group resolves the start; if the track has already ended with no matching groups, it sends SUBSCRIBE_END with no preceding SUBSCRIBE_OK.
A rejection is a stream reset: a publisher that cannot serve the subscription MUST promptly reset the stream rather than leave it pending, so a subscriber distinguishes "pending" from "refused" by the reset, not by a timeout.

The track's immutable publisher properties are not carried here; they are fetched once via a [Track Stream](#track-stream).
The subscriber needs the track's TRACK_INFO (notably its timescale) to interpret FRAME messages, and MAY open the Track and Subscribe streams concurrently, buffering frames until it arrives.

The publisher closes the stream (FIN) only once every group from start to end has been accounted for, either via a Group Stream (completed or reset) or a SUBSCRIBE_DROP message.
This MAY occur after SUBSCRIBE_END, since stragglers within the range can still be dropped.
Unbounded subscriptions stay open until SUBSCRIBE_END, and either endpoint MAY reset the stream at any time.

### Fetch
A subscriber opens a Fetch Stream (0x3) to request a single Group from a Track.

The subscriber sends a FETCH message containing the broadcast path, track name, priority, and group sequence.
The publisher responds with FRAME messages directly on the same bidirectional stream; there is no response header, and the group sequence is implicit from the request.
As with a subscription, the subscriber MUST already have the track's [TRACK_INFO](#track-info) to parse the returned frames.
The publisher FINs the stream after the last frame, or resets the stream on error.

Fetch behaves like HTTP: a single request/response per stream.

### Track {#track-stream}
A subscriber opens a Track Stream (0x6) to learn a Track's immutable publisher properties without subscribing or fetching.

The subscriber sends a TRACK message containing the broadcast path and track name.
The publisher replies with a single TRACK_INFO message and then FINs the stream, or resets the stream on error (e.g. the track does not exist).
The returned properties are fixed for the lifetime of the track, so the subscriber SHOULD cache TRACK_INFO and reuse it across every SUBSCRIBE and FETCH for that track.
The cached value is tied to the broadcast's original publisher, identified by the first entry of the advertisement's reconstructed path: a re-announce that preserves the first entry keeps it valid, while one that changes it replaces the broadcast and the subscriber MUST discard and re-request it (see [ANNOUNCE_RESTART](#announce-restart)).
A subscriber that reached the track without an advertisement has no such invalidation signal and SHOULD NOT cache TRACK_INFO beyond a single connection.

Because a subscriber cannot parse buffered group frames until TRACK_INFO arrives, the publisher SHOULD prioritize TRACK_INFO ahead of group data on the connection.

### Probe
A subscriber opens a Probe Stream (0x4) to measure, and optionally increase, the available bitrate of the connection.
The publisher advertises its Probe level in SETUP (see [Probe Parameter](#probe-parameter)): None, Report (measure only), or Increase (measure and actively probe).

The subscriber sends a PROBE message with a target bitrate on the bidirectional stream.
The subscriber MAY send additional PROBE messages on the same stream to update the target bitrate; the publisher MUST treat each PROBE as a new target to attempt.
If the publisher advertised the Increase capability, it SHOULD pad the connection (or send redundant data) to achieve the most recent target bitrate, without exceeding the congestion window.
A publisher that advertised Report but not Increase ignores the target and only reports; it MUST NOT pad above its current sending rate.
In either case the publisher periodically replies with PROBE messages on the same bidirectional stream containing the current estimated bitrate and smoothed RTT.

If the publisher advertised no Probe capability (e.g., the congestion controller is not exposed), it MUST reset the stream.

### Goaway
Either endpoint can open a Goaway Stream (0x5) to initiate a graceful session shutdown.

The sender sends a GOAWAY message containing an optional new session URI.
If the URI is non-empty, the peer SHOULD establish a new session at the provided URI and migrate any active subscriptions.
The peer MUST NOT open new streams on the current session after receiving a GOAWAY.

The sender closes the stream (FIN) when it is ready to terminate the session.
The peer SHOULD close all streams and the session after migrating or when it no longer needs the session.

# Delivery
The most important concept in moq-lite is how to deliver a subscription.
QUIC can only improve the user experience if data is delivered out-of-order during congestion.
This is the sole reason why data is divided into Broadcasts, Tracks, Groups, and Frames.

moq-lite consists of multiple groups being transmitted in parallel across separate streams.
How these streams get transmitted over the network is very important, and yet has been distilled down into a few simple properties:

## Prioritization
The Publisher and Subscriber both exchange `Priority` and `Ordered` values:
- `Priority` determines which Track should be transmitted next.
- `Ordered` determines which Group within the Track should be transmitted next.

A publisher SHOULD attempt to transmit streams based on these fields.
This depends on the QUIC implementation and it may not be possible to get fine-grained control.

### Priority
The `Subscriber Priority` is scoped to the connection and MAY change over the life of the subscription via SUBSCRIBE_UPDATE.
The `Publisher Priority` is fixed for the lifetime of the Track (see [TRACK_INFO](#track-info)) and SHOULD be used only to resolve conflicts or ties.

A conflict can occur when a relay tries to serve multiple downstream subscriptions from a single upstream subscription.
The relay cannot pick any one subscriber's priority, so the upstream subscription SHOULD use the publisher priority instead of some combination of different subscriber priorities.
Publisher priority is therefore mostly relevant on the upstream (origin-facing) leg of a relay; closer to the subscriber, the subscriber priority dominates.

Rather than try to explain everything, here's an example:

**Example:**
There are two people in a conference call, Ali and Bob.

We subscribe to both of their audio tracks with subscriber priority 2 and video tracks with subscriber priority 1.
Each publisher advertises a fixed publisher priority (here audio at 2 and video at 1) used only to break ties.
This results in equal priority for `Ali` and `Bob` while prioritizing audio.
```text
ali/audio + bob/audio: subscriber_priority=2 publisher_priority=2
ali/video + bob/video: subscriber_priority=1 publisher_priority=1
```

Because publisher priority cannot change, dynamic adaptation is the subscriber's job.
If the subscriber detects that Bob is actively speaking, it raises the subscriber priority of Bob's tracks via SUBSCRIBE_UPDATE:
```text
bob/audio: subscriber_priority=4 publisher_priority=2
bob/video: subscriber_priority=3 publisher_priority=1
ali/audio: subscriber_priority=2 publisher_priority=2
ali/video: subscriber_priority=1 publisher_priority=1
```

The subscriber priority takes precedence, so the subscriber can likewise full-screen Ali's window by raising the subscriber priority of Ali's tracks above Bob's.

### Ordered
The `Subscriber Ordered` field signals if older (0x1) or newer (0x0) groups should be transmitted first within a Track.
The `Publisher Ordered` field MAY likewise be used to resolve conflicts.

An application SHOULD use `ordered` when it wants to provide a VOD-like experience, preferring to buffer old groups rather than skip them.
An application SHOULD NOT use `ordered` when it wants to provide a live experience, preferring to skip old groups rather than buffer them.

Note that [expiration](#expiration) is not affected by `ordered`.
An old group may still be cancelled/skipped if it exceeds the `Subscriber Max Latency`.
An application MUST support gaps and out-of-order delivery even when `ordered` is true.


## Expiration
Expiration governs when an older group is dropped.
The publisher SHOULD reset Group Streams for non-latest groups whose age relative to the latest group exceeds `Subscriber Max Latency` (see [SUBSCRIBE](#subscribe)); the subscriber MAY also locally drop such groups.
Expiration only removes the group from live delivery; the publisher MAY still retain it for FETCH or new subscriptions until its age exceeds `Publisher Max Latency` (see [TRACK_INFO](#track-info)).

It is not crucial to aggressively expire groups thanks to [prioritization](#prioritization), but a lower priority group still consumes RAM, bandwidth, and potentially flow control.
It is RECOMMENDED that an application set conservative limits and only resort to expiration when data is absolutely no longer needed.

A group is never expired until at least the next group (by sequence number) has been received or queued.
Once a newer group exists, the group's age is measured two ways, and it is expired once **either** measure exceeds the relevant `Max Latency`:

- **Timestamp age**: the difference between this group's first frame timestamp and the first frame timestamp of the latest group that has at least one frame. This measure is consistent across relays and unaffected by buffering or jitter.
- **Wall-clock age**: the difference between when this group's first byte arrived (subscriber) or was queued (publisher) and the same instant for the latest group.

The two backstop each other: a publisher cannot keep stale groups alive with fresh-looking timestamps, and a burst of groups arriving together does not reset their age.
A group that contains zero frames has no timestamp, so only the wall-clock age applies; this avoids stalling expiration on empty groups sent as keep-alives or gap markers.

An expired group SHOULD be reset at the QUIC level to avoid consuming flow control.

## Unidirectional Streams
Unidirectional streams are used for data transmission.

|--------|----------|-------------|
|     ID | Stream   | Creator     |
|-------:|:---------|-------------|
|    0x0 | Group    | Publisher   |
| ------ | -------- | ----------- |
|    0x1 | Setup    | Either      |
| ------ | -------- | ----------- |

### Setup {#setup-stream}
Each endpoint MUST open a Setup Stream (0x1) at the start of the session to advertise the optional capabilities and extensions it supports.

The opener sends a single SETUP message and immediately closes the stream (FIN).
There is exactly one Setup Stream per direction; an endpoint that receives a second Setup Stream MUST close the session with a PROTOCOL_VIOLATION.
An endpoint with no optional capabilities sends a SETUP with an empty parameter list rather than omitting the stream, giving the peer a deterministic signal that no capabilities are forthcoming.

See the [Session](#session) section for how an endpoint avoids waiting on the peer's SETUP before exchanging other streams.

### Group
A publisher creates Group Streams in response to a Subscribe Stream.

A Group Stream MUST start with a GROUP message and MAY be followed by any number of FRAME messages.
A Group MAY contain zero FRAME messages, potentially indicating a gap in the track.
A frame MAY contain an empty payload, potentially indicating a gap in the group.

Both the publisher and subscriber MAY reset the stream at any time.
This is not a fatal error and the session remains active.
The subscriber MAY cache the error and potentially retry later.

## Datagrams
QUIC datagrams provide unreliable, unordered delivery for latency-sensitive content that does not need retransmission.

A publisher MAY transmit any Group as a single QUIC datagram in addition to (or instead of) opening a Group Stream, based on application hints, group size, and network conditions.
A datagram-delivered group contains exactly one Frame and is not cached or retransmitted; a publisher SHOULD only send a datagram if the congestion controller can transmit it immediately.
There is no separate subscription for datagram delivery: datagrams are routed to existing subscriptions via the Subscribe ID, and a subscriber receiving the same group via both a stream and a datagram MUST deduplicate by group sequence.

Each datagram body has the following encoding (note: there is no message length prefix; the QUIC datagram boundary delimits the payload):

~~~
DATAGRAM Body {
  Subscribe ID (i)
  Group Sequence (i)
  Timestamp (i)
  Payload (b)
}
~~~

**Subscribe ID**:
The Subscribe ID of an active subscription on the same session.
A subscriber receiving a datagram with an unknown Subscribe ID MUST silently drop it.

**Group Sequence**:
The absolute sequence number of the group carried by this datagram.

**Timestamp**:
The absolute timestamp of the single frame in the group, expressed in the Track's `Timescale`.
Any varint value (including 0) is valid.

**Payload**:
The frame payload, extending to the end of the datagram.
The total datagram body MUST NOT exceed 1200 bytes, ensuring it fits within the minimum QUIC path MTU without IP-layer fragmentation; a publisher MUST NOT send a larger one and a receiver MUST silently drop it.
A group whose frame does not fit is simply not eligible for datagram delivery.



# Encoding
This section covers the encoding of each message.

## Message Length
Most messages are prefixed with a variable-length integer indicating the number of bytes in the message payload that follows.
This length field does not include the length of the varint length itself.

An implementation SHOULD close the connection with a PROTOCOL_VIOLATION if it receives a message with an unexpected length.
The version and extensions should be used to support new fields, not the message length.

## STREAM_TYPE {#stream_type}
All streams start with a short header indicating the stream type.

~~~
STREAM_TYPE {
  Stream Type (i)
}
~~~

The stream ID depends on if it's a bidirectional or unidirectional stream, as indicated in the Streams section.
A receiver MUST reset the stream if it receives an unknown stream type.
Unknown stream types MUST NOT be treated as fatal; this is the fallback when an extension stream is opened against a peer that did not negotiate it.


## SETUP {#setup}
A SETUP message advertises the optional capabilities and extensions the sender supports for this session.
It is sent exactly once, as the only message on a [Setup Stream](#setup-stream).

~~~
SETUP Message {
  Message Length (i)
  Parameter Count (i)
  Setup Parameter (..) ...
}

Setup Parameter {
  Parameter ID (i)
  Parameter Length (i)
  Parameter Value (..)
}
~~~

**Parameter Count**:
The number of Setup Parameters that follow.

**Parameter ID**:
Identifies the capability or extension.
A receiver MUST ignore unknown Parameter IDs, allowing new capabilities to be added without breaking older implementations.
A Parameter ID MUST NOT appear more than once; a receiver MUST close the session with a PROTOCOL_VIOLATION if it does.

**Parameter Length**:
The length of Parameter Value in bytes.

**Parameter Value**:
The parameter-specific value, interpreted according to Parameter ID.

A capability is available for the session only if the relevant endpoint advertises it; an absent parameter means the sender does not support that capability.
The following Setup Parameters are defined:

|------|-----------|-------------|
|  ID  | Name      | Value       |
|-----:|:----------|:------------|
| 0x1  | Probe     | Level (i)   |
|------|-----------|-------------|
| 0x2  | Path      | Path (s)    |
|------|-----------|-------------|
| 0x3  | Role      | Role (i)    |
|------|-----------|-------------|
| 0x4  | Cost      | Cost (i)    |
|------|-----------|-------------|
| 0x5  | Origin    | Hop ID (i)  |
|------|-----------|-------------|

### Probe Parameter {#probe-parameter}
The Probe Parameter advertises the sender's capability level when acting as a publisher on a [Probe Stream](#probe).
The Parameter Value is a variable-length integer level, where each level includes the one below it:

- `0` **None**: The publisher does not support probing. Equivalent to omitting the parameter.
- `1` **Report**: The publisher can measure and periodically report its estimated bitrate.
- `2` **Increase**: The publisher can additionally pad the connection (or send redundant data) to probe for bandwidth above its current sending rate, up to the subscriber's target.

A subscriber MUST consult the publisher's advertised level before relying on a Probe Stream:

- At `None`, the subscriber SHOULD NOT open a Probe Stream; if it does, the publisher MUST reset it.
- At `Report`, the subscriber MAY open a Probe Stream to monitor the estimated bitrate but MUST NOT expect the publisher to pad above its current sending rate. A subscriber that needs to probe for additional bandwidth MUST use an alternative (e.g. speculatively switching to a higher rendition).
- At `Increase`, the subscriber MAY request a target bitrate and expect the publisher to actively probe up to it.

### Path Parameter {#path-parameter}
The Path Parameter carries the request target the client wishes to reach, equivalent to the path and query components of a moq-lite URI.
A server uses it to route the session before any broadcasts are exchanged; its interpretation is otherwise application-defined and opaque to moq-lite.

The Parameter Value is a UTF-8 string holding the `path-abempty` component of the URI [RFC3986]; when the URI carries a query, the client MUST append `?` followed by the `query` component, since a deployment commonly puts the session credential there.
The value MAY be empty, which is equivalent to omitting the parameter: both request the server's default path.
A server that receives an invalid value MUST close the session with a PROTOCOL_VIOLATION, and one that does not recognize the requested path MUST close the session.

This parameter exists for the bindings that negotiate only an ALPN token and have no request URI of their own (bindings 1 and 3 in [Transports](#transports)); a client using one of them SHOULD send it.
It MUST NOT be sent on a binding whose handshake carries a request URI (bindings 2 and 4), and only the client sends it; a receiver MUST close the session with a PROTOCOL_VIOLATION on either violation.
A relay MUST NOT forward it; like other per-hop setup metadata it applies only to this hop.

### Role Parameter {#role-parameter}
The Role Parameter advertises the direction the client intends to use the session for.
A moq-lite session is bidirectional, but a client's authorization (e.g. the credential in the [Path](#path-parameter)) may grant only one direction; the hint lets a server reject a mismatched session during SETUP instead of accepting it and silently carrying no data.

The Parameter Value is a variable-length integer:

- `0` **Both**: The client may publish and/or subscribe. The default, and equivalent to omitting the parameter.
- `1` **Publisher**: The client intends to publish (and not subscribe).
- `2` **Subscriber**: The client intends to subscribe (and not publish).

A receiver that does not recognize the value MUST treat it as `Both`, so a newer client cannot break an older server.
The role is a hint that only ever narrows the session: a server MUST still enforce the client's authorization on every publish and subscribe, and MAY close a session whose advertised role requires a direction the authorization does not grant.

Only the client sends it; a client that receives one MUST close the session with a PROTOCOL_VIOLATION. A relay MUST NOT forward it.

### Cost Parameter {#cost-parameter}
The Cost Parameter declares the routing cost of this connection: each endpoint adds the value to the Route Cost of every announcement it receives over the connection (see [Routing](#routing)).

The Parameter Value is a variable-length integer in deployment-chosen units, the same units as the Route Cost.
An absent parameter means the default cost of 1, under which the accumulated Route Cost equals the hop count and routing degenerates to shortest-path.
A value of 0 is meaningful and distinct from absent: it makes the link free, e.g. between two relays in the same datacenter.

Only the client sends it, so both ends charge the same link the same amount; a server MUST NOT send it and a relay MUST NOT forward it.

### Origin Parameter {#origin-parameter}
The Origin Parameter declares the sender's Hop ID: the identity it stamps onto announcements it forwards.
The Parameter Value is a variable-length integer; a value of 0 carries no identity and is equivalent to omitting the parameter.

Declaring it at setup gives the receiver the peer's identity before any other stream arrives, so route selection applies the same exclusion to the peer's subscriptions as to its announcements (see [Routing](#routing)), even on a session that never opens an Announce Stream.
Either endpoint MAY send it; a subscriber-only endpoint with no identity MAY omit it, but a publisher SHOULD have a Hop ID regardless (see [ANNOUNCE_OK](#announce-ok)).
A relay MUST NOT forward it.


## ANNOUNCE_REQUEST {#announce-request}
A subscriber sends an ANNOUNCE_REQUEST message to indicate it wants to receive announcements for any broadcasts with a path that starts with the requested prefix.

~~~
ANNOUNCE_REQUEST Message {
  Message Length (i)
  Broadcast Path Prefix (s),
}
~~~

**Broadcast Path Prefix**:
Indicate interest for any broadcasts with a path that starts with this prefix.

The publisher MUST respond with an ANNOUNCE_OK message followed by ANNOUNCE_START messages for any matching and available broadcasts, followed by ANNOUNCE_START, ANNOUNCE_END, and ANNOUNCE_RESTART messages for any future updates, subject to [Routing](#routing).
Implementations SHOULD consider reasonable limits on the number of matching broadcasts to prevent resource exhaustion.


## ANNOUNCE_OK {#announce-ok}
A publisher sends an ANNOUNCE_OK message exactly once, as the first message on the response side of an Announce Stream.
It carries metadata that is constant for the lifetime of the stream and applies to every announcement that follows.

~~~
ANNOUNCE_OK Message {
  Message Length (i)
  Hop ID (i)
  Active Count (i)
}
~~~

**Hop ID**:
The publisher's own Hop ID.
This is treated as the implicit trailing entry of every ANNOUNCE_START and ANNOUNCE_RESTART Hop ID list on this stream; those messages MUST NOT repeat this value as the last entry of their `Hop ID` list.
The value 0 is reserved to mean "unknown": either no Hop ID was assigned (e.g. when bridging from an older protocol version) or the endpoint deliberately withholds it to obscure the underlying routing.
A publisher that assigns a Hop ID MUST choose a non-zero value, and SHOULD assign itself one (a fresh random value per session suffices): a broadcast whose path starts with 0 loses restart continuity and failover, since 0 proves nothing shared (see [Routing](#routing) and [ANNOUNCE_RESTART](#announce-restart)).
Receivers reconstruct the full path as `Hop IDs ++ [ANNOUNCE_OK.Hop ID]`.

**Active Count**:
The number of ANNOUNCE_START messages that the publisher will send immediately as the initial set.
The subscriber MAY block reporting any announcement to the application until all `Active Count` initial announcements have arrived, then deliver the initial set as a batch.
Any announcements beyond `Active Count` are live updates and SHOULD be reported as they arrive.
A value of `0` is valid and means the publisher is offering no initial available broadcasts; all subsequent announcements (if any) are live updates.


## ANNOUNCE_START {#announce-start}
A publisher sends an ANNOUNCE_START message to advertise that a broadcast is available.
Each ANNOUNCE_START implicitly assigns the next Announce ID on the stream, later referenced by ANNOUNCE_END and ANNOUNCE_RESTART (see [Announce](#announce)).

Only the suffix is encoded on the wire, as the full path can be constructed by prepending the requested prefix.

~~~
ANNOUNCE_START Message {
  Type (i) = 0x0
  Message Length (i)
  Broadcast Path Suffix (s),
  Hop Count (i),
  Hop ID (i) ...,
  Route Cost (i),
}
~~~

**Type**:
Set to 0x0 to indicate an ANNOUNCE_START message.

**Broadcast Path Suffix**:
This is combined with the broadcast path prefix to form the full broadcast path.

**Hop Count**:
The number of Hop ID entries that follow, NOT including the publisher's own `Hop ID` from ANNOUNCE_OK.
A value of 0 means no Hop ID entries are present, indicating either that the announcement originated locally on the publisher (the publisher itself is the origin) or that the upstream peer does not support hop tracking.
A receiver MUST close the stream with a PROTOCOL_VIOLATION if the Hop Count does not match the number of subsequent Hop ID entries.

**Hop ID**:
A unique identifier for each relay in the path from the origin publisher, ordered from origin to the upstream of the responding publisher.
The responding publisher's own Hop ID is NOT included in this list; it is carried once in ANNOUNCE_OK, so the total path length is `Hop Count + 1`.
When forwarding an announcement received from an upstream peer, a relay MUST append the upstream peer's ANNOUNCE_OK `Hop ID` to this list, since that ID is no longer implicit downstream.
The first entry of the reconstructed path identifies the original publisher of the broadcast; ANNOUNCE_RESTART uses it to distinguish a route change from a replacement (see [ANNOUNCE_RESTART](#announce-restart)).
A Hop ID value of 0 means the hop is unknown: either it was never assigned or a relay deliberately withholds it (see [Routing](#routing)).

**Route Cost**:
The marginal cost of subscribing to the broadcast via this advertisement, in units chosen by the deployment.
The original publisher seeds the value with its production cost: 0 for content it is already producing, larger for content it would have to start producing on demand (e.g. a standby transcoder).
When forwarding an announcement received from an upstream peer, a relay adds the cost of the link the announcement arrived on (see [Cost Parameter](#cost-parameter)), saturating rather than wrapping so an absurd upstream value ranks last instead of overflowing to best.

A relay that is actively carrying the broadcast (a live subscription exists for at least one of its tracks) SHOULD advertise 0 instead of the accumulated value: its ingress is already paid for, which is what lets a cluster deduplicate onto a warm copy.
The discount applies only to the path the relay actually serves from; a standby path keeps its accumulated value, since serving from it means opening a fresh ingest.
When the relay stops carrying the broadcast it SHOULD restore the accumulated value via ANNOUNCE_RESTART, optionally after a grace period so brief churn does not flap routing.

Two relays that independently begin carrying the same broadcast would each see the other's 0 as cheaper than its own source, and both switching at once would leave the broadcast with no source.
Before re-parenting onto a 0-cost advertisement from another actively-carrying relay (one whose path has two or more entries), a relay SHOULD apply a deterministic tie-break, such as comparing a hash of the broadcast path and each Hop ID, so exactly one side moves.
Cheaper advertisements from anything else carry no such hazard and SHOULD be adopted immediately.


## ANNOUNCE_END {#announce-end}
A publisher sends an ANNOUNCE_END message to retract a previously started broadcast, referencing its Announce ID.
The id is retired and MUST NOT be referenced again.

~~~
ANNOUNCE_END Message {
  Type (i) = 0x1
  Message Length (i)
  Announce ID (i)
}
~~~

**Type**:
Set to 0x1 to indicate an ANNOUNCE_END message.

**Announce ID**:
The ordinal implicitly assigned by a prior ANNOUNCE_START on this stream.
Referencing an id that was never assigned, or one already retired, is a protocol violation.
Announce IDs are never reused within a stream; a broadcast that is announced again after an ANNOUNCE_END gets a fresh id from its next ANNOUNCE_START.


## ANNOUNCE_RESTART {#announce-restart}
A publisher sends an ANNOUNCE_RESTART message to atomically replace a previously started broadcast, referencing its Announce ID.
The advertisement is replaced in place (equivalent to ANNOUNCE_END+ANNOUNCE_START) and the id stays live.
The Hop ID list MAY differ from the original (e.g. after a relay failover or upstream restart).

The first entry of the reconstructed path identifies the original publisher (see [ANNOUNCE_START](#announce-start)), and it determines what the restart means:

- The first entry is unchanged and non-zero: the same publisher's broadcast is reachable over a different route.
  The broadcast's content and track properties are continuous, so cached TRACK_INFO stays valid (see [Track](#track-stream)) and the subscriber MAY resume in-flight subscriptions on the new route at a group boundary instead of resubscribing.
- The first entry changed, or is 0: a different publisher may have replaced the broadcast at this path.
  0 identifies nothing, so continuity can never be proven for it.
  The subscriber MUST treat it as a new broadcast: cached TRACK_INFO MUST be discarded, and existing subscriptions do not carry over (the group sequences and track set of the new broadcast are unrelated to the old one).

The first entry only identifies the publisher, not a particular broadcast instance: a publisher that restarts its own broadcast (same path, new content) is indistinguishable from a route change.
A future extension may add an explicit epoch to announcements to make that case detectable.

~~~
ANNOUNCE_RESTART Message {
  Type (i) = 0x2
  Message Length (i)
  Announce ID (i),
  Hop Count (i),
  Hop ID (i) ...,
  Route Cost (i),
}
~~~

**Type**:
Set to 0x2 to indicate an ANNOUNCE_RESTART message.

**Announce ID**:
The ordinal implicitly assigned by a prior ANNOUNCE_START on this stream.
Referencing an id that was never assigned, or one already retired by an ANNOUNCE_END, is a protocol violation.

**Hop Count**, **Hop ID**, and **Route Cost**:
As defined for [ANNOUNCE_START](#announce-start).
A restart whose only change is the Route Cost is valid: it is how a relay advertises that it started or stopped actively carrying the broadcast.


## SUBSCRIBE
SUBSCRIBE is sent by a subscriber to start a subscription.

~~~
SUBSCRIBE Message {
  Message Length (i)
  Subscribe ID (i)
  Broadcast Path (s)
  Track Name (s)
  Subscriber Priority (8)
  Subscriber Ordered (8)
  Subscriber Max Latency (i)
  Group Start (i)
  Group End (i)
}
~~~

**Subscribe ID**:
A unique identifier chosen by the subscriber.
A Subscribe ID MUST NOT be reused within the same session, even if the prior subscription has been closed.

**Subscriber Priority**:
The priority of the subscription within the session, represented as a u8.
The publisher SHOULD transmit *higher* values first during congestion.
See the [Prioritization](#prioritization) section for more information.

**Subscriber Ordered**:
A single byte representing whether groups are transmitted in ascending (0x1) or descending (0x0) order.
The publisher SHOULD transmit *older* groups first during congestion if true.
See the [Prioritization](#prioritization) section for more information.

**Subscriber Max Latency**:
The subscriber's preference, in milliseconds, for how long a non-latest group may remain in flight before being considered stale and dropped from live delivery.
The publisher SHOULD reset (at the QUIC level) Group Streams for groups whose age relative to the latest group exceeds this duration.
Applies only to non-latest groups; the latest group is never dropped on staleness grounds.
A value of `0` means the subscriber wants only the latest group in live delivery (older groups are immediately stale once a newer group arrives).
This is a delivery-time preference, not a retention rule: the publisher MAY still hold these groups for FETCH or future subscriptions (see `Publisher Max Latency` in [TRACK_INFO](#track-info)).
See the [Expiration](#expiration) section for more information.

**Group Start**:
The first group to deliver.
A value of 0 means the latest group (default).
A non-zero value is the absolute group sequence + 1.

**Group End**:
The last group to deliver (inclusive).
A value of 0 means unbounded (default).
A non-zero value is the absolute group sequence + 1.

`Group Start` and `Group End` are offset by 1 only so 0 can mean "absent"; every other group field in this document is a plain absolute sequence.


## SUBSCRIBE_UPDATE
A subscriber can modify a subscription with a SUBSCRIBE_UPDATE message.
A subscriber MAY send multiple SUBSCRIBE_UPDATE messages to update the subscription.
The start and end group can be changed in either direction (growing or shrinking).

~~~
SUBSCRIBE_UPDATE Message {
  Message Length (i)
  Subscriber Priority (8)
  Subscriber Ordered (8)
  Subscriber Max Latency (i)
  Group Start (i)
  Group End (i)
}
~~~

See [SUBSCRIBE](#subscribe) for information about each field.


## TRACK
TRACK is sent by a subscriber to request a Track's immutable publisher properties.
It is the first message on a Track Stream (0x6).

~~~
TRACK Message {
  Message Length (i)
  Broadcast Path (s)
  Track Name (s)
}
~~~

**Broadcast Path**:
The broadcast path of the track.

**Track Name**:
The name of the track.

## TRACK_INFO {#track-info}
TRACK_INFO is sent by the publisher in response to a TRACK message.
It is the sole message on the Track Stream; the publisher FINs immediately afterward, or resets the stream on error (e.g. the track does not exist).

~~~
TRACK_INFO Message {
  Message Length (i)
  Publisher Priority (8)
  Publisher Ordered (8)
  Publisher Max Latency (i)
  Timescale (i)
}
~~~

Every field is **fixed for the lifetime of the Track** and MUST NOT change; a change requires a new Track (a re-announcement of the broadcast).
This is what lets the properties live on their own stream, fetched once and cached, instead of being echoed on every SUBSCRIBE and FETCH response.
Publisher properties fan *out* at a relay (one upstream subscription serving many downstreams), so a change would have to propagate everywhere; subscriber properties fan *in*, which the relay already merges, so they MAY change freely via SUBSCRIBE_UPDATE.

**Publisher Priority**:
The publisher's priority for this Track, represented as a u8, used only to resolve ties between subscriptions of equal subscriber priority.
See the [Prioritization](#prioritization) section for more information.

**Publisher Ordered**:
The publisher's group ordering preference (ascending `0x1` or descending `0x0`), used only to resolve ties.
See the [Prioritization](#prioritization) section for more information.

**Publisher Max Latency**:
The maximum age, in milliseconds, that the publisher caches a non-latest group past the arrival of a newer group.
Applies only to non-latest groups; the latest group is always retained.
It is an upper bound on retention, the inverse of an HTTP `Cache-Control: max-age` guarantee:

- A subscriber MAY issue a SUBSCRIBE or FETCH with an older `Group Start`, but the publisher MAY have already dropped any group whose age exceeds `Publisher Max Latency`.
- The publisher MAY drop groups sooner than `Publisher Max Latency` under resource pressure; subscribers MUST NOT assume older groups within the bound are still available.

A value of `0` means the publisher caches only the latest group (older groups MAY be dropped as soon as a newer group arrives).
The unit is milliseconds, matching `Subscriber Max Latency`.
See the [Expiration](#expiration) section for more information.

**Timescale**:
The number of timestamp units per second for frame timestamps on this Track.
It MUST be non-zero; a subscriber that receives 0 MUST reset the stream with a protocol violation.
Common values include `1000` (milliseconds), `1000000` (microseconds), `48000` (audio sample rate), and `90000` (RTP video clock).

## SUBSCRIBE_OK {#subscribe-ok}
A SUBSCRIBE_OK message confirms a subscription and resolves its absolute start group.
It is the first message the publisher sends on the Subscribe Stream, once the start group is known.

This is the trimmed-down counterpart of MoqTransport's SUBSCRIBE_OK: it retains the name and the role of the publisher's positive response, but carries only the resolved start group (all other per-track properties live in [TRACK_INFO](#track-info)).

~~~
SUBSCRIBE_OK Message {
  Type (i) = 0x0
  Message Length (i)
  Group (i)
}
~~~

**Type**:
Set to 0x0 to indicate a SUBSCRIBE_OK message.

**Group**:
The absolute sequence number of the first group that will be delivered.
It MUST be greater than or equal to the requested start group; any groups in between are unavailable and implicitly dropped, with no separate SUBSCRIBE_DROP required.
A subscriber that requested the latest group learns the resolved sequence here.

## SUBSCRIBE_END {#subscribe-end}
A SUBSCRIBE_END message is sent by the publisher to signal that no group at or after a given sequence will be produced.

~~~
SUBSCRIBE_END Message {
  Type (i) = 0x1
  Message Length (i)
  Group (i)
}
~~~

**Type**:
Set to 0x1 to indicate a SUBSCRIBE_END message.

**Group**:
The exclusive end of the range: the absolute sequence number of the first group that will never be delivered.
A value of 0 means the track ended before producing any groups.
The subscriber MUST NOT wait for any group at or after this sequence.

SUBSCRIBE_END bounds the range but does not by itself end the stream: the publisher MAY still send SUBSCRIBE_DROP for groups below this sequence that it cannot deliver, and FINs the stream only once every group below this sequence has been accounted for.

## SUBSCRIBE_DROP
A SUBSCRIBE_DROP message is sent by the publisher on the Subscribe Stream when groups cannot be served.
It MAY arrive at any point after the subscription is opened, including after SUBSCRIBE_END for stragglers within the resolved range (a leading range is instead dropped implicitly by SUBSCRIBE_OK).

~~~
SUBSCRIBE_DROP Message {
  Type (i) = 0x2
  Message Length (i)
  Group Start (i)
  Group End (i)
  Error Code (i)
}
~~~

**Type**:
Set to 0x2 to indicate a SUBSCRIBE_DROP message.

**Group Start**:
The first absolute group sequence in the dropped range.

**Group End**:
The last absolute group sequence in the dropped range (inclusive).

**Error Code**:
An application-specific error code.
A value of 0 indicates no error; the groups are simply unavailable.

## FETCH
FETCH is sent by a subscriber to request a single group from a track.

~~~
FETCH Message {
  Message Length (i)
  Broadcast Path (s)
  Track Name (s)
  Subscriber Priority (8)
  Group Sequence (i)
}
~~~

**Broadcast Path**:
The broadcast path of the track to fetch from.

**Track Name**:
The name of the track to fetch from.

**Subscriber Priority**:
The priority of the fetch within the session, represented as a u8.
See the [Prioritization](#prioritization) section for more information.

**Group Sequence**:
The sequence number of the group to fetch.

The publisher responds with FRAME messages directly on the same stream; there is no response header, and the group sequence is implicit from the FETCH request.
The publisher FINs the stream after the last frame, or resets on error; there is no FETCH_ERROR message.

## PROBE
PROBE is used to measure the available bitrate of the connection.

~~~
PROBE Message {
  Message Length (i)
  Bitrate (i)
  RTT (i)
}
~~~

**Bitrate**:
When sent by the subscriber (stream opener): the target bitrate in bits per second that the publisher should pad up to.
The publisher only honors a target above its current sending rate if it advertised the Increase capability (see [Probe Parameter](#probe-parameter)); otherwise the target is ignored and the publisher only reports.
When sent by the publisher (responder): the current estimated bitrate in bits per second.
A value of 0 means unknown.

**RTT**:
The smoothed round-trip time in milliseconds, as defined in [RFC9002].
A value of 0 means unknown.

> NOTE: RTT is included in the PROBE message because not all QUIC implementations and browser WebTransport APIs expose RTT statistics directly. This field may be deprecated once RTT is universally available via the underlying transport API.

## GOAWAY
A GOAWAY message is sent to initiate a graceful session shutdown with an optional redirect.

~~~
GOAWAY Message {
  Message Length (i)
  New Session URI (s)
}
~~~

**New Session URI**:
A URI for the peer to reconnect to.
An empty string indicates no redirect; the peer should simply close the session.
A recipient MUST validate the URI against local policy before reconnecting, including verifying the scheme, authority, and port are permitted.
If validation fails, the recipient MUST close the session without reconnecting.

## GROUP
The GROUP message contains information about a Group, as well as a reference to the subscription being served.

~~~
GROUP Message {
  Message Length (i)
  Subscribe ID (i)
  Group Sequence (i)
}
~~~

**Subscribe ID**:
The corresponding Subscribe ID.
This ID is used to distinguish between multiple subscriptions for the same track.

**Group Sequence**:
The sequence number of the group.
This SHOULD increase by 1 for each new group.
A subscriber MUST handle gaps, potentially caused by congestion.


## FRAME
The FRAME message is a payload within a group.

~~~
FRAME Message {
  Timestamp Delta (i)
  Message Length (i)
  Payload (b)
}
~~~

**Timestamp Delta**:
A signed delta from the previous frame's timestamp, in the Track's negotiated `Timescale`.
Encoded as a zigzag-mapped variable-length integer:

- Encode: `unsigned = (signed << 1) ^ (signed >> 63)` (arithmetic right shift).
- Decode: `signed = (unsigned >> 1) ^ -(unsigned & 1)`.

Zigzag interleaves non-negative and negative values so small magnitudes of either sign fit in a 1-byte varint.
The first frame of a group is delta-encoded from `0`, so its `Timestamp Delta` is the zigzag encoding of the absolute timestamp.

**Payload**:
An application-specific payload.
The `Message Length` describes the payload size on the wire.


# Appendix A: Changelog

## moq-lite-06
- Excluded the reserved Hop ID 0 from publisher identity everywhere it implies continuity: advertisements whose first entry is 0 are never interchangeable, and an ANNOUNCE_RESTART whose first entry is 0 replaces the broadcast rather than continuing it. Publishers SHOULD assign themselves a Hop ID (a random per-session value suffices) to keep restart continuity and failover.
- Moved the Qmux-over-WebSocket binding details to draft-lcurley-qmux-websocket; the binding itself is unchanged.
- Extended the SETUP `Path` parameter to carry the URI query: a client appends `?` and the query component after the path, matching moq-transport's PATH option. The credential a deployment puts in the query was previously unrepresentable on a binding with no request URI.
- Allowed an empty SETUP `Path` parameter, equivalent to omitting it; both request the server's default path. Previously an empty value was a protocol violation, which made the two ways of asking for the default disagree.
- Corrected SUBSCRIBE_END `Group` to an exclusive bound: the first sequence that will never be delivered, with 0 meaning no groups were produced. It was previously specified as the inclusive last group, which could not distinguish an empty track from one whose only group was 0.
- Split ANNOUNCE_BROADCAST into three typed messages: ANNOUNCE_START (0x0), ANNOUNCE_END (0x1), and ANNOUNCE_RESTART (0x2), each prefixed with a Type discriminator like the subscribe stream's responses.
- Added implicit Announce IDs: each ANNOUNCE_START assigns the next per-stream ordinal.
- ANNOUNCE_END and ANNOUNCE_RESTART reference the Announce ID instead of repeating the broadcast path.
- Replaced the duplicate-`active` restart idiom with ANNOUNCE_RESTART; a second ANNOUNCE_START for an already-available path is now a protocol violation.
- Defined the first entry of the reconstructed path as the original publisher's identity: a restart that preserves it is a route change (TRACK_INFO stays valid, subscriptions may resume), one that changes it replaces the broadcast (TRACK_INFO discarded, nothing resumes).
- Added a `Route Cost` field to ANNOUNCE_START and ANNOUNCE_RESTART: the accumulated cost of the transfers a subscription via this advertisement would newly cause. Route selection prefers the lowest cost, with path length as the tie-break, and the most recently received advertisement below that.
- Added a SETUP `Cost` parameter (0x4) declaring the price a link adds to every announcement crossing it; unpriced links default to 1, degrading to shortest-path routing.
- Removed `Exclude Hop` from ANNOUNCE_REQUEST. The receiver's hop-based loop check already discards a looped announcement, so the field only saved the wasted send.
- Stated the receiver's loop check normatively in ANNOUNCE_START: an announcement whose reconstructed path contains the receiver's own Hop ID is neither forwarded nor selected as a route.
- Added a SETUP `Origin` parameter (0x5): each endpoint declares its Hop ID at session setup, carrying session-wide the identity `Exclude Hop` carried per announce stream, and filtering subscriptions as well as announcements (including sessions that never open an Announce Stream).
- Made advertisement selection per subscriber: the publisher advertises the best path avoiding each subscriber's declared origin (a subscriber the serving path flows through receives the best standby instead of nothing), MUST serve subscriptions by the same exclusion, and the actively-carrying cost discount applies only to the serving path. This is how redundant (shared first hop) publishers fail over across a mesh.
- Defined same-path advertisements sharing a first entry as interchangeable content a relay may splice across at a Group boundary; differing first entries never splice, the later replacing the earlier.

## moq-lite-05
- Renamed ANNOUNCE_INTEREST to ANNOUNCE_REQUEST and ANNOUNCE to ANNOUNCE_BROADCAST.
- Added a SETUP message and Setup Stream (0x1).
- Added a SETUP `Probe` parameter.
- Added a SETUP `Path` parameter to convey the request path on bindings that have no request URI (native QUIC and Qmux-over-TCP/TLS).
- Added a SETUP `Role` parameter so a client can advertise its intended direction (Publisher/Subscriber/Both) and be rejected during SETUP when its authorization lacks that direction.
- Added Track Stream (0x6) and TRACK_INFO.
- Removed FETCH_OK.
- Trimmed SUBSCRIBE_OK to a single resolved start group.
- Split end-of-subscription signaling into SUBSCRIBE_END.
- Renamed `Start Group`/`End Group` to `Group Start`/`Group End` in SUBSCRIBE, SUBSCRIBE_UPDATE, and SUBSCRIBE_DROP.
- Allowed duplicate `active` ANNOUNCE_BROADCAST messages to atomically replace the prior advertisement.
- Added ANNOUNCE_OK with `Hop ID` and `Active Count`.
- Added mandatory `Timescale` to TRACK_INFO.
- Added `Timestamp Delta` to FRAME.
- Added `Timestamp` to the QUIC datagram body.
- Moved `Publisher Max Latency` to TRACK_INFO and redefined it as a maximum retention bound: the longest the publisher caches a non-latest group (the inverse of an HTTP `Cache-Control: max-age` guarantee). `Subscriber Max Latency` keeps its name and remains the subscriber's delivery-time expiration preference.
- Expire a group once **either** its timestamp age or its wall-clock arrival age exceeds Max Latency (the shorter lifetime wins), bounding both manipulated timestamps and delivery bursts.
- Added QUIC datagram delivery for groups. Datagrams and Group Streams are independent delivery modes with no conversion between them: an oversized (>1200 byte) datagram MUST NOT be sent and is dropped on receipt, and bindings without a datagram channel do not fall back from datagrams to streams.
- Added Qmux [qmux] transport bindings for TCP/TLS and WebSocket.

## moq-lite-04
- Renamed ANNOUNCE_PLEASE to ANNOUNCE_SUBSCRIBE.
- ANNOUNCE_BROADCAST `Hops` count replaced with explicit `Hop ID` list for loop detection.
- Added `Exclude Hop` to ANNOUNCE_REQUEST for relay loop avoidance.
- Added GOAWAY stream for graceful session shutdown and migration.
- Added RTT to PROBE message. Bitrate and RTT use 0 for unknown.

## moq-lite-03
- Version negotiated via ALPN (`moq-lite-xx`) instead of SETUP messages.
- Removed Session, SessionCompat streams and SESSION_CLIENT/SESSION_SERVER/SESSION_UPDATE messages.
- Unknown stream types reset instead of fatal; enables extension negotiation via stream probing.
- Added FETCH stream for single group download.
- Added Start Group and End Group to SUBSCRIBE, SUBSCRIBE_UPDATE, and SUBSCRIBE_OK.
- Added SUBSCRIBE_DROP on Subscribe stream.
- Subscribe stream closed (FIN) when all groups accounted for.
- Added PROBE stream replacing SESSION_UPDATE bitrate.
- Removed ANNOUNCE_INIT message.
- Added `Hops` to ANNOUNCE_BROADCAST.
- Added `Subscriber Max Latency` and `Subscriber Ordered` to SUBSCRIBE and SUBSCRIBE_UPDATE.
- Added `Publisher Priority`, `Publisher Max Latency`, and `Publisher Ordered` to SUBSCRIBE_OK.
- SUBSCRIBE_OK may be sent multiple times.

## moq-lite-02
- Added SessionCompat stream.
- Editorial stuff.

## moq-lite-01
- Added Message Length (i) to all messages.

# Appendix B: Upstream Differences
A quick comparison of moq-lite and moq-transport-14:

- Streams instead of request IDs.
- Pull only: No unsolicited publishing.
- FETCH is HTTP-like (single request/response) vs MoqTransport FETCH (multiple groups).
- Capabilities negotiated via a SETUP message on a unidirectional stream that does not block other streams, instead of MoqTransport's blocking CLIENT_SETUP/SERVER_SETUP handshake on the control stream.
- Both moq-lite and MoqTransport use ALPN for version identification.
- Names use utf-8 strings instead of byte arrays.
- Track Namespace is a string, not an array of any array of bytes.
- Subscriptions default to the latest group, not the latest object.
- No subgroups
- No group/object ID gaps
- No object properties
- No paused subscriptions (forward=0)

## Deleted Messages
- MAX_SUBSCRIBE_ID
- REQUESTS_BLOCKED
- SUBSCRIBE_ERROR
- UNSUBSCRIBE
- PUBLISH_DONE
- PUBLISH
- PUBLISH_OK
- PUBLISH_ERROR
- FETCH_OK
- FETCH_ERROR
- FETCH_CANCEL
- FETCH_HEADER
- TRACK_STATUS
- TRACK_STATUS_OK
- TRACK_STATUS_ERROR
- PUBLISH_NAMESPACE
- PUBLISH_NAMESPACE_OK
- PUBLISH_NAMESPACE_ERROR
- PUBLISH_NAMESPACE_CANCEL
- SUBSCRIBE_NAMESPACE_OK
- SUBSCRIBE_NAMESPACE_ERROR
- UNSUBSCRIBE_NAMESPACE
- OBJECT_DATAGRAM

## Renamed Messages
- SUBSCRIBE_NAMESPACE -> ANNOUNCE_REQUEST
- SUBGROUP_HEADER -> GROUP

## Deleted Fields
Some of these fields occur in multiple messages.

- Request ID
- Track Alias
- Group Order
- Filter Type
- StartObject
- Expires
- ContentExists
- Largest Group ID
- Largest Object ID
- Parameters
- Subgroup ID
- Object ID
- Object Status
- Extension Headers


# Security Considerations
moq-lite inherits the transport security of the underlying connection: QUIC and WebTransport provide confidentiality and integrity via TLS 1.3, and the Qmux bindings run over TLS (TCP) or a `wss://` WebSocket. How that connection is authenticated is out of scope (see [Connection](#connection)). The considerations below are specific to moq-lite.

## Bandwidth Probing
The `Increase` Probe level (see [Probe Parameter](#probe-parameter)) lets a subscriber ask the publisher to pad the connection up to a target bitrate. A publisher MUST NOT treat the target as authorization to send beyond what congestion control allows: padding is bounded by the congestion window, so probing cannot be used to amplify traffic toward the subscriber or a spoofed address. A publisher that only advertised `Report` MUST NOT pad above its current sending rate. Because all data flows on an established, congestion-controlled session to the connecting peer, moq-lite offers no off-path amplification vector.

## Session Redirection
GOAWAY carries an optional New Session URI that asks the peer to reconnect elsewhere. A malicious or compromised peer could use this to redirect a client to an attacker-controlled server. A recipient MUST validate the URI against local policy (scheme, authority, and port) before reconnecting, and MUST NOT reconnect if validation fails (see [GOAWAY](#goaway)). Migrated subscriptions carry no implicit trust from the prior session; the new session is authenticated independently.

## Routing Metadata and Privacy
Hop IDs (see [ANNOUNCE_OK](#announce-ok) and [ANNOUNCE_START](#announce-start)) expose the relay path of a broadcast, which may reveal internal topology. A relay that does not wish to disclose its position MAY use the reserved value 0 ("unknown") instead of a stable identifier, at the cost of losing loop detection through itself (see [Routing](#routing)). The origin-based announcement filter (see [Origin Parameter](#origin-parameter)) exists for loop avoidance, not access control: a subscriber cannot verify that a publisher honored it, so it MUST NOT be relied upon to hide a broadcast from a peer that declared its origin.

## Resource Exhaustion
A peer can open many streams (subscriptions, announcements, fetches) or request large announce prefixes. Implementations SHOULD bound the number of concurrent subscriptions, announce matches, and cached groups, and SHOULD rely on QUIC flow control and stream limits to backpressure a misbehaving peer (see [ANNOUNCE_REQUEST](#announce-request)). Expiration (see [Expiration](#expiration)) bounds how long stale groups consume memory and flow control.

## Datagram Injection
Datagrams are routed to a subscription solely by Subscribe ID and carry no per-group authentication beyond that of the QUIC connection. On an unmodified QUIC/WebTransport connection this is sufficient, since datagrams are protected by the transport. A subscriber MUST silently drop any datagram with an unknown Subscribe ID and MUST deduplicate against groups received on streams (see [Datagrams](#datagrams)).

## Opaque Payloads
The moq-lite layer treats Frame payloads as opaque and performs no validation of their contents. Confidentiality or integrity of the media itself (e.g. end-to-end encryption transparent to relays) is an application concern and out of scope for this draft.


# IANA Considerations

This document has no IANA actions.


--- back

# Acknowledgments
{:numbered="false"}

TODO acknowledge.
