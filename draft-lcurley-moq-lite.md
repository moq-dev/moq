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

The bindings negotiated solely via ALPN (bare QUIC and Qmux over TCP/TLS) have no request URI, so a client conveys the request path it wishes to reach via the [Path Parameter](#path-parameter) in SETUP.
The remaining bindings carry the path in their own handshake and do not use this parameter; see [Path Parameter](#path-parameter).

When UDP is unavailable, moq-lite-05 MAY also run over reliable byte-stream transports via Qmux [qmux].
Qmux provides a length-delimited polyfill for QUIC streams on top of TCP/TLS or WebSocket; see [Transports](#transports) for the specific bindings and ALPN negotiation.

The session is active immediately after the QUIC/WebTransport connection is established.
Both endpoints SHOULD begin sending and receiving streams right away to avoid an extra round-trip.

Optional capabilities and extensions are negotiated via a SETUP message (see [SETUP](#setup)).
Each endpoint MUST open a unidirectional Setup Stream at the start of the session, send a single SETUP message advertising what it supports, and immediately close the stream (FIN); an endpoint with no optional capabilities sends a SETUP with an empty parameter list.
The two SETUP messages are independent; neither endpoint waits for the peer's SETUP before opening other streams.
Because a SETUP is always sent, the buffering below is bounded: an endpoint knows the peer's full capability set has arrived once it receives that single SETUP.
An endpoint SHOULD continue to send and process non-Setup streams until a negotiated extension would change the behavior or encoding of a stream, in which case it MUST buffer that stream until the peer's SETUP has been received.
For example, if an extension adds a field to SUBSCRIBE_OK, the subscriber buffers SUBSCRIBE_OK until SETUP arrives so the new field can be parsed.

As a fallback, an endpoint that opens an extension stream the peer does not support simply sees that stream reset (see [STREAM_TYPE](#stream_type)).
A negotiated capability applies only to this hop; each session is negotiated independently and relays MUST NOT forward SETUP.

While moq-lite is a point-to-point protocol, it's intended to work end-to-end via relays.
Each client establishes a session with a CDN edge server, ideally the closest one.
Any broadcasts and subscriptions are transparently proxied by the CDN behind the scenes.

## Broadcast
A Broadcast is a collection of Tracks from a single publisher.
This corresponds to a MoqTransport's "track namespace".

A publisher may produce multiple broadcasts, each of which is advertised via an ANNOUNCE_BROADCAST message.
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

A Group MAY also be transmitted as a single QUIC datagram (see [Datagrams](#datagrams)) when the entire group fits in one datagram and reliability is not required.
A datagram-delivered group contains exactly one Frame and is not retransmitted on loss.
The same subscription MAY receive groups via both streams and datagrams; the application MUST be prepared to deduplicate by group sequence.

## Frame
A Frame is a payload of bytes within a Group.

A frame is used to represent a chunk of data with an upfront size.
The contents are opaque to the moq-lite layer.

Each frame carries a presentation timestamp expressed in the parent Track's `Timescale` (units per second, part of the [TRACK_INFO](#track-info)).
Every Track has a media timeline — the `Timescale` is always non-zero and every frame is timestamped.
The timestamp is the source-of-truth for media time and is one of the two inputs to the moq-lite layer's [expiration](#expiration) decisions, alongside wall-clock arrival time.

# Flow
This section outlines the flow of messages within a moq-lite session.
See the Messages section for the specific encoding.

## Connection
moq-lite runs on top of any transport that provides ordered, multiplexed, bidirectional streams.
The primary transports are bare QUIC and WebTransport over HTTP/3.
WebTransport is a layer on top of QUIC and HTTP/3, required for web support.
The API is nearly identical to QUIC with the exception of stream IDs.

When UDP is unavailable, moq-lite-05 also runs over Qmux [qmux], a length-delimited polyfill that maps QUIC streams onto a reliable byte-stream transport.
See [Transports](#transports) for the supported bindings.

How the underlying connection is authenticated is out-of-scope for this draft.

## Transports {#transports}
moq-lite-05 defines four transport bindings.
All four carry the same control and data streams defined elsewhere in this document; they differ only in how QUIC streams are multiplexed onto the underlying connection.

|----|---------------------|------------------|----------------------|
|    | Transport           | ALPN / Identifier | Record framing      |
|---:|:--------------------|:------------------|:--------------------|
| 1  | QUIC                | `moq-lite-05`     | Native QUIC streams |
| 2  | WebTransport / H3   | `moq-lite-05` (CONNECT header) | Native WebTransport streams |
| 3  | Qmux over TCP/TLS   | `moq-lite-05` (ALPN over TLS)  | Qmux Record [qmux]  |
| 4  | Qmux over WebSocket | `moq-lite-05` (Sec-WebSocket-Protocol) | WebSocket message |

For bindings 1 and 2, moq-lite uses the underlying QUIC/WebTransport stream APIs directly.
QUIC datagrams (see [Datagrams](#datagrams)) are supported by bindings 1 and 2 only.
Bindings 3 and 4 are reliable byte-stream transports and have no datagram channel; a publisher MUST NOT emit datagrams on those bindings. Groups are still delivered normally via Group Streams; there is no conversion of a datagram into a stream.

### Qmux over TCP/TLS
A client opens a TCP connection, performs a TLS handshake, and negotiates the ALPN token `moq-lite-05`.
Each direction of the TLS byte stream then carries Qmux Records as defined in [qmux], which encapsulate QUIC STREAM frames.
The Qmux Record's `Size` field length-delimits each record on the byte stream:

~~~
QMux Record {
  Size (i),
  Frames (..)
}
~~~

All other moq-lite semantics (stream types, message encoding, flow control, etc.) are identical to native QUIC.

### Qmux over WebSocket
Qmux as published does not define a WebSocket binding due to IETF working-group charter scope.
This section specifies how moq-lite-05 maps Qmux onto WebSocket [RFC6455]; the mapping is straightforward because both layers provide length-delimited messages over a reliable byte stream.

A client opens a WebSocket connection with the `Sec-WebSocket-Protocol` header set to `moq-lite-05`.
Each WebSocket binary message carries exactly one Qmux Record's `Frames` payload — that is, one or more QUIC frames concatenated.
The WebSocket message length replaces the Qmux Record `Size` field: the WebSocket framing layer already self-delimits each record, so the `Size` varint MUST NOT be transmitted and MUST NOT be expected by the receiver.

In other words, a Qmux-over-WebSocket record is:

~~~
WebSocket Binary Message {
  Frames (..)
}
~~~

Text messages MUST NOT be used and MUST be treated as a protocol violation.
All other Qmux semantics (in-order STREAM frame delivery, stream IDs, etc.) apply unchanged.

WebSocket ping/pong frames are handled by the WebSocket layer and are independent of moq-lite.

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
The publisher replies with a single ANNOUNCE_OK message followed by ANNOUNCE_BROADCAST messages for any matching broadcasts and any future changes.

ANNOUNCE_OK carries metadata that applies to every ANNOUNCE_BROADCAST on this stream and is sent exactly once at the start of the response:

- The publisher's own `Hop ID`, which is the implicit trailing entry of every ANNOUNCE_BROADCAST's Hop ID list. Hoisting it out of every ANNOUNCE_BROADCAST saves bytes since it is identical for every announcement on the session.
- The number of `active` ANNOUNCE_BROADCAST messages (`Active Count`) the publisher will send immediately as the initial set. The subscriber MAY buffer until all `Active Count` initial announcements arrive before reporting them to the application, avoiding a trickle. Any ANNOUNCE_BROADCAST messages beyond `Active Count` are live updates and SHOULD be reported to the application as they arrive.

Each ANNOUNCE_BROADCAST message contains one of the following statuses:

- `active`: a matching broadcast is available.
- `ended`: a previously `active` broadcast is no longer available.

Each broadcast starts as `ended`.
An `active` announcement makes the broadcast available; a subsequent `ended` makes it unavailable again.

A publisher SHOULD advertise only the best path it knows for each broadcast.
If the best path changes (e.g. a relay failover or upstream restart), the publisher MAY send another `active` for that broadcast: the new announcement atomically replaces the prior one (equivalent to UNANNOUNCE+ANNOUNCE_BROADCAST).
A publisher MUST NOT keep multiple `active` advertisements for the same broadcast on the same stream — each broadcast has at most one current advertisement at a time.
A subscriber that sees the same broadcast advertised across multiple streams SHOULD route subscriptions to the advertisement with the shortest total path length (see [ANNOUNCE_BROADCAST](#announce-broadcast)).

The subscriber MUST reset the stream if it receives an `ended` for a broadcast that is not currently `active`, or any ANNOUNCE_BROADCAST before ANNOUNCE_OK.
When the stream is closed, the subscriber MUST assume that all broadcasts are now `ended`.

Path prefix matching and equality is done on a byte-by-byte basis.
There MAY be multiple Announce Streams, potentially containing overlapping prefixes, that get their own ANNOUNCE_OK + ANNOUNCE_BROADCAST messages.

### Subscribe
A subscriber opens Subscribe Streams to request a Track.

The subscriber MUST start a Subscribe Stream with a SUBSCRIBE message followed by any number of SUBSCRIBE_UPDATE messages.
When a start group can be resolved, the publisher replies with a SUBSCRIBE_OK message (confirming the subscription and resolving its start group), followed by any number of SUBSCRIBE_END and SUBSCRIBE_DROP messages.
When the accepted track has already ended with no matching groups there is no start group to resolve, so the publisher sends SUBSCRIBE_END with no preceding SUBSCRIBE_OK.
A rejection is a stream reset: if the publisher cannot serve the subscription — the track does not exist, or it otherwise refuses — it MUST reset the stream rather than leave it pending, and SHOULD do so promptly (within roughly a round trip) so the subscriber is not left waiting.
A subscription the publisher accepts but has no groups for yet is not a rejection: for a live track the publisher MAY withhold SUBSCRIBE_OK until the first matching group resolves the start. A subscriber therefore distinguishes "pending" from "refused" by the stream reset, not by a timeout.
The Subscribe Stream does not carry the track's publisher properties — those are immutable and fetched once via a [Track Stream](#track-stream) (see [TRACK_INFO](#track-info)).
The subscriber MUST have the track's TRACK_INFO before it can fully interpret the FRAME messages that arrive on Group Streams, since the timescale is needed to interpret each timestamp; it MAY open the Track and Subscribe streams concurrently and buffer frames until TRACK_INFO arrives.

The publisher sends SUBSCRIBE_OK once the absolute start group is resolved, and SUBSCRIBE_END once no further groups will be produced (see [SUBSCRIBE_OK](#subscribe-ok) and [SUBSCRIBE_END](#subscribe-end)).
The publisher closes the stream (FIN) only once every group from start to end has been accounted for, either via a GROUP stream (completed or reset) or a SUBSCRIBE_DROP message.
This MAY occur after SUBSCRIBE_END, since stragglers within the range can still be dropped.
Unbounded subscriptions (no end group) stay open until the publisher sends SUBSCRIBE_END (and accounts for the remaining groups) to indicate the track has ended, or either endpoint resets.
Either endpoint MAY reset/cancel the stream at any time.

### Fetch
A subscriber opens a Fetch Stream (0x3) to request a single Group from a Track.

The subscriber sends a FETCH message containing the broadcast path, track name, priority, and group sequence.
The publisher responds with FRAME messages directly on the same bidirectional stream — there is no response header.
The Subscribe ID and Group Sequence for the returned FRAME messages are implicit, taken from the original FETCH request.
As with a subscription, the subscriber MUST already have the track's [TRACK_INFO](#track-info) to parse the returned frames; because the properties are immutable, a single Track Stream lookup is reused across every FETCH of that track (group-by-group fetches do not re-fetch it).
The publisher FINs the stream after the last frame, or resets the stream on error.

Fetch behaves like HTTP: a single request/response per stream.

### Track {#track-stream}
A subscriber opens a Track Stream (0x6) to learn a Track's immutable publisher properties without subscribing or fetching.

The subscriber sends a TRACK message containing the broadcast path and track name.
The publisher replies with a single TRACK_INFO message and then FINs the stream, or resets the stream on error (e.g. the track does not exist).
The returned properties are fixed for the lifetime of the track, so the subscriber SHOULD cache TRACK_INFO and reuse it across every SUBSCRIBE and FETCH for that track rather than requesting it again.
When the track was discovered via an ANNOUNCE_BROADCAST, the cached value is tied to that advertisement: if the broadcast is re-announced (a new `active` ANNOUNCE_BROADCAST that atomically replaces the prior one), the subscriber MUST discard the cached TRACK_INFO and MUST re-request it before parsing any further FRAME messages, since the timescale may have changed.
If FRAME messages cannot be decoded against the cached TRACK_INFO (for example a malformed delta or payload after a missed re-announcement), the subscriber MUST reset the affected stream with a protocol violation and re-request TRACK_INFO.
A subscriber that reached the track without an advertisement (e.g. a path known out of band) has no such invalidation signal; it MAY re-request TRACK_INFO whenever it needs to confirm freshness (for example on a new session). A stale cache only risks misparsing frames from a changed track, so the subscriber that cannot observe re-announcements SHOULD NOT cache TRACK_INFO beyond a single connection.

Because a subscriber MAY open the Track stream concurrently with a SUBSCRIBE or FETCH (see [Subscribe](#subscribe) and [Fetch](#fetch)) and cannot parse any buffered group frames until TRACK_INFO arrives, the publisher SHOULD prioritize TRACK_INFO ahead of group data on the connection.
Otherwise the concurrent case — intended to keep a cold subscribe or fetch to a single round trip — would stall behind queued group frames that the subscriber cannot yet decode.

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
Each publisher advertises a fixed publisher priority — here audio at 2 and video at 1 — used only to break ties.
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
Expiration governs when an older group is dropped from a live subscription's Group Stream(s).
It is primarily a delivery-time concern, governed by `Subscriber Max Latency`.
Whether older groups remain available for FETCH or future subscriptions is governed by `Publisher Max Latency`, an upper bound on how long the publisher caches a non-latest group; beyond that the publisher MAY drop it.

It is not crucial to aggressively expire groups thanks to [prioritization](#prioritization).
However, a lower priority group will still consume RAM, bandwidth, and potentially flow control.
It is RECOMMENDED that an application set conservative limits and only resort to expiration when data is absolutely no longer needed.

The publisher SHOULD reset Group Streams for non-latest groups whose age relative to the latest group exceeds the `Subscriber Max Latency` value in SUBSCRIBE/SUBSCRIBE_UPDATE.
The subscriber MAY also locally drop such groups for its own resource accounting.
Expiration only removes the group from the live subscription's stream; the publisher MAY still retain it for FETCH or new subscriptions until its age exceeds `Publisher Max Latency` (see [TRACK_INFO](#track-info)).

Group age is computed relative to the latest group by sequence number.
A group is never expired until at least the next group (by sequence number) has been received or queued.
Once a newer group exists, the group's age is measured two ways, and it is expired once **either** measure exceeds the relevant `Max Latency` (`Subscriber Max Latency` for live delivery, `Publisher Max Latency` for the publisher's cache):

- **Timestamp age**: the difference between this group's first frame timestamp and the first frame timestamp of the latest group that has at least one frame (see [Frame](#frame)). This measure is consistent across relays and unaffected by buffering or jitter.
- **Wall-clock age**: the difference between when this group's first byte arrived (subscriber) or was queued (publisher) and the same instant for the latest group.

Equivalently, a group's effective lifetime is the *minimum* of the two — whichever clock declares it stale first wins. The two measures backstop each other:

- A publisher cannot keep stale groups alive by reporting timestamps that look fresh; the wall-clock age expires them anyway.
- A burst of groups delivered close together (e.g. catching up at the start of a subscription) does not reset their age; the timestamp age still expires the media-old ones even though they all just arrived.

A group that contains zero frames has no timestamp, so only the wall-clock age applies.
This avoids stalling expiration on tracks that intentionally emit empty groups as keep-alives or gap markers.

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

A publisher MAY transmit any Group as a single QUIC datagram in addition to (or instead of) opening a Group Stream.
Datagrams are not cached: a publisher SHOULD only send a datagram if the congestion controller can transmit it immediately.
A subscriber receiving the same group via both a stream and a datagram MUST deduplicate by group sequence.

There is no separate subscription for datagram delivery; datagrams are routed to existing subscriptions via the Subscribe ID.
The publisher decides which groups to send as datagrams based on application hints, group size, and network conditions.
A subscriber that does not wish to receive datagrams can ignore them; well-behaved publishers SHOULD avoid sending datagrams when streams suffice.

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
Each datagram represents a complete group containing exactly one frame.

**Timestamp**:
The absolute timestamp of the single frame in the group, expressed in the Track's negotiated `Timescale`.
Any varint value (including 0) is a valid absolute timestamp.

**Payload**:
The frame payload, extending to the end of the datagram.
The total datagram body (including all header fields above and the payload) MUST NOT exceed 1200 bytes.
This limit ensures the datagram fits within the minimum QUIC path MTU without IP-layer fragmentation.
A publisher MUST NOT send a datagram whose body exceeds this limit, and a receiver MUST silently drop any datagram that does.
A group whose frame does not fit is simply not eligible for datagram delivery; it is delivered as a Group Stream like any other group, which is not a conversion of the datagram.



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

|------|----------|-------------|
|  ID  | Name     | Value       |
|-----:|:---------|:------------|
| 0x1  | Probe    | Level (i)   |
|------|----------|-------------|
| 0x2  | Path     | Path (s)    |
|------|----------|-------------|

### Probe Parameter {#probe-parameter}
The Probe Parameter advertises the sender's capability level when acting as a publisher on a [Probe Stream](#probe).
The Parameter Value is a variable-length integer level, where each level includes the one below it:

- `0` **None**: The publisher does not support probing. Equivalent to omitting the parameter.
- `1` **Report**: The publisher can measure and periodically report its estimated bitrate.
- `2` **Increase**: The publisher can additionally pad the connection (or send redundant data) to probe for bandwidth above its current sending rate, up to the subscriber's target.

The levels are nested rather than independent: probing for more bandwidth is meaningless without measuring it, so Increase always includes Report. Reporting the current bitrate is far simpler to implement, so a publisher may support Report without Increase.

A subscriber MUST consult the publisher's advertised level before relying on a Probe Stream:

- At `None`, the subscriber SHOULD NOT open a Probe Stream; if it does, the publisher MUST reset it.
- At `Report`, the subscriber MAY open a Probe Stream to monitor the estimated bitrate but MUST NOT expect the publisher to pad above its current sending rate. A subscriber that needs to probe for additional bandwidth MUST use an alternative (e.g. speculatively switching to a higher rendition).
- At `Increase`, the subscriber MAY request a target bitrate and expect the publisher to actively probe up to it.

### Path Parameter {#path-parameter}
The Path Parameter carries the request path the client wishes to reach, equivalent to the path component of a moq-lite URI.
A server uses it to route the session to the correct origin, relay, or virtual host before any broadcasts are exchanged; its interpretation is otherwise application-defined and opaque to moq-lite.
Unlike the capability-style Setup Parameters, it is per-hop setup metadata that rides along in SETUP because that is the first client-to-server message of the session.

The Parameter Value is a non-empty UTF-8 string that begins with `/` and uses the path syntax of a URI [RFC3986].

This parameter exists for bindings that have no request URI of their own: the native QUIC binding (binding 1 in [Transports](#transports)) and the Qmux-over-TCP/TLS binding (binding 3), both of which negotiate only an ALPN token.
The remaining bindings convey the path in their own handshake.

- A client using a binding without a request URI (binding 1 or 3) MUST send exactly one Path Parameter in its SETUP.
- The Path Parameter MUST NOT be sent on a binding that carries a request URI. The WebTransport (binding 2) and Qmux-over-WebSocket (binding 4) bindings convey the path in their handshake URI (the CONNECT request path and the WebSocket request URI, respectively). A server that receives a Path Parameter on either of these bindings MUST close the session with a PROTOCOL_VIOLATION.
- A server MUST NOT send a Path Parameter. SETUP is bidirectional, but the path is meaningful only from client to server; a client that receives a Path Parameter MUST close the session with a PROTOCOL_VIOLATION.
- A server that receives a Path that is empty or is not a valid URI path MUST close the session with a PROTOCOL_VIOLATION. A server that does not recognize or support the requested path MUST close the session.

A relay MUST NOT forward the Path Parameter; like other per-hop setup metadata it applies only to this hop (see [Session](#session)).


## ANNOUNCE_REQUEST {#announce-request}
A subscriber sends an ANNOUNCE_REQUEST message to indicate it wants to receive an ANNOUNCE_BROADCAST message for any broadcasts with a path that starts with the requested prefix.

~~~
ANNOUNCE_REQUEST Message {
  Message Length (i)
  Broadcast Path Prefix (s),
  Exclude Hop (i),
}
~~~

**Broadcast Path Prefix**:
Indicate interest for any broadcasts with a path that starts with this prefix.

**Exclude Hop**:
If non-zero, the publisher SHOULD skip ANNOUNCE_BROADCAST messages for broadcasts whose Hop ID entries (including the publisher's own `Hop ID` from ANNOUNCE_OK) contain this value.
This is used by relays to avoid routing loops in a cluster.

The publisher MUST respond with an ANNOUNCE_OK message followed by ANNOUNCE_BROADCAST messages for any matching and active broadcasts, followed by ANNOUNCE_BROADCAST messages for any future updates.
Implementations SHOULD consider reasonable limits on the number of matching broadcasts to prevent resource exhaustion.


## ANNOUNCE_OK {#announce-ok}
A publisher sends an ANNOUNCE_OK message exactly once, as the first message on the response side of an Announce Stream.
It carries metadata that is constant for the lifetime of the stream and applies to every ANNOUNCE_BROADCAST that follows.

~~~
ANNOUNCE_OK Message {
  Message Length (i)
  Hop ID (i)
  Active Count (i)
}
~~~

**Hop ID**:
The publisher's own Hop ID.
This is treated as the implicit trailing entry of every ANNOUNCE_BROADCAST's Hop ID list on this stream; ANNOUNCE_BROADCAST messages MUST NOT repeat this value as the last entry of their `Hop ID` list.
The value 0 is reserved to mean "unknown": either no Hop ID was assigned (e.g. when bridging from an older protocol version) or the endpoint deliberately withholds it to obscure the underlying routing.
A publisher that assigns a Hop ID MUST choose a non-zero value.
Receivers reconstruct the full path as `ANNOUNCE_BROADCAST.Hop IDs ++ [ANNOUNCE_OK.Hop ID]`.

**Active Count**:
The number of `active` ANNOUNCE_BROADCAST messages that the publisher will send immediately as the initial set.
The subscriber MAY block reporting any announcement to the application until all `Active Count` initial ANNOUNCEs have arrived, then deliver the initial set as a batch.
Any ANNOUNCE_BROADCAST messages beyond `Active Count` are live updates and SHOULD be reported as they arrive.
A value of `0` is valid and means the publisher is offering no initial active broadcasts; all subsequent ANNOUNCEs (if any) are live updates.


## ANNOUNCE_BROADCAST {#announce-broadcast}
A publisher sends an ANNOUNCE_BROADCAST message to advertise a change in broadcast availability.
Only the suffix is encoded on the wire, as the full path can be constructed by prepending the requested prefix.

The status is relative to all prior ANNOUNCE_BROADCAST messages for the same path on the same stream.
A publisher MAY send an `active` for a path that is already `active`: the new announcement atomically replaces the prior one, including any change to the Hop ID list.
An `ended` MUST follow a corresponding `active`; an `ended` for a path that is not currently `active` is a protocol violation.
An ANNOUNCE_BROADCAST before ANNOUNCE_OK is a protocol violation.

~~~
ANNOUNCE_BROADCAST Message {
  Message Length (i)
  Announce Status (i),
  Broadcast Path Suffix (s),
  Hop Count (i),
  Hop ID (i) ...,
}
~~~

**Announce Status**:
A flag indicating the announce status.

- `ended` (0): A path is no longer available.
- `active` (1): A path is now available. If the path is already `active`, this announcement atomically replaces the prior one — the Hop ID list MAY differ (e.g. after a relay failover or upstream restart).

**Broadcast Path Suffix**:
This is combined with the broadcast path prefix to form the full broadcast path.

**Hop Count**:
The number of Hop ID entries that follow, NOT including the publisher's own `Hop ID` from ANNOUNCE_OK.
A value of 0 means no Hop ID entries are present, indicating either that the announcement originated locally on the publisher (the publisher itself is the origin) or that the upstream peer does not support hop tracking.
A receiver MUST close the stream with a PROTOCOL_VIOLATION if the Hop Count does not match the number of subsequent Hop ID entries.

**Hop ID**:
A unique identifier for each relay in the path from the origin publisher, ordered from origin to the upstream of the responding publisher.
The responding publisher's own Hop ID is NOT included in this list; it is carried once in ANNOUNCE_OK as `Hop ID`.
When forwarding an announcement received from an upstream peer, a relay MUST append the upstream peer's ANNOUNCE_OK `Hop ID` to this list (since that ID is no longer implicit downstream) and then send its own `Hop ID` in the ANNOUNCE_OK it sends to the downstream subscriber.
The total path length is `Hop Count + 1` (including the implicit ANNOUNCE_OK `Hop ID`).
A Hop ID value of 0 means the hop is unknown: either it was never assigned (e.g. when bridging from an older protocol version) or a relay deliberately withholds it to obscure the underlying routing; the Hop Count still reflects the total number of entries including unknown hops.


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
This immutability is what lets the properties live on their own stream — fetched once and cached — instead of being echoed on every SUBSCRIBE and FETCH response.
It is also deliberate for relays: a relay serving one upstream subscription to many downstream subscribers would otherwise have to fan a single publisher-side change out to every downstream (and invalidate any cached groups) — publisher changes fan *out*.
Subscriber properties (see [SUBSCRIBE](#subscribe)) are the opposite: they fan *in* at the relay, which already merges them, so they MAY change freely via SUBSCRIBE_UPDATE.

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
It MUST be non-zero: every Track has a media timeline, so every FRAME carries a `Timestamp Delta` and every datagram body carries a `Timestamp` (see [FRAME](#frame) and [Datagrams](#datagrams)).
A subscriber that receives a `Timescale` of 0 MUST reset the Subscribe or Fetch stream with a protocol violation.
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
This is a plain absolute sequence, **not** the `absolute + 1` form used by `Group Start` in SUBSCRIBE (see [SUBSCRIBE](#subscribe)); decode the requested `Group Start` before comparing.
Once decoded, this MUST be greater than or equal to the requested start group.
If it is strictly greater, the groups in between are unavailable and will not be delivered; SUBSCRIBE_OK thus also acts as an implicit drop of that leading range, and no separate SUBSCRIBE_DROP is required for it.
A subscriber that requested the latest group (`Group Start` = 0) learns the resolved sequence here.

## SUBSCRIBE_END {#subscribe-end}
A SUBSCRIBE_END message is sent by the publisher to signal that no group after a given sequence will be produced.

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
The absolute sequence number of the last group that may be delivered (inclusive).
This is a plain absolute sequence, **not** the `absolute + 1` form used by `Group Start`/`Group End` in SUBSCRIBE.
The subscriber MUST NOT wait for any group after this sequence.
SUBSCRIBE_END bounds the range but does not by itself end the stream: the publisher MAY still send SUBSCRIBE_DROP for groups at or below this sequence that it cannot deliver, and FINs the stream only once every group up to this sequence has been accounted for.

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
Despite the shared field name, this is a plain absolute sequence, **not** the `absolute + 1` form used by `Group Start` in SUBSCRIBE.

**Group End**:
The last absolute group sequence in the dropped range (inclusive).
As with `Group Start` above, this is a plain absolute sequence, **not** the `absolute + 1` form used in SUBSCRIBE.

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

The publisher responds with FRAME messages directly on the same stream — there is no response header.
The subscriber parses them using the track's [TRACK_INFO](#track-info), which it MUST already have (see the [Track Stream](#track-stream)); the group sequence is implicit from the FETCH request.
The publisher FINs the stream after the last frame, or resets on error.
There is no FETCH_ERROR message — the publisher signals failure by resetting the stream.

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

Zigzag interleaves non-negative and negative values (`0 → 0, -1 → 1, 1 → 2, -2 → 3, 2 → 4, ...`) so small magnitudes of either sign fit in a 1-byte varint and there is exactly one wire encoding for zero.
The first frame of a group is delta-encoded from `0`, so its `Timestamp Delta` is the zigzag encoding of the absolute timestamp.

**Payload**:
An application-specific payload.
The `Message Length` describes the payload size on the wire.


# Appendix A: Changelog

## moq-lite-05
- Renamed ANNOUNCE_INTEREST to ANNOUNCE_REQUEST and ANNOUNCE to ANNOUNCE_BROADCAST.
- Added a SETUP message and Setup Stream (0x1).
- Added a SETUP `Probe` parameter.
- Added a SETUP `Path` parameter to convey the request path on bindings that have no request URI (native QUIC and Qmux-over-TCP/TLS).
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
GOAWAY carries an optional New Session URI that asks the peer to reconnect elsewhere. A malicious or compromised peer could use this to redirect a client to an attacker-controlled server. A recipient MUST validate the URI against local policy — scheme, authority, and port — before reconnecting, and MUST NOT reconnect if validation fails (see [GOAWAY](#goaway)). Migrated subscriptions carry no implicit trust from the prior session; the new session is authenticated independently.

## Routing Metadata and Privacy
Hop IDs (see [ANNOUNCE_OK](#announce-ok) and [ANNOUNCE_BROADCAST](#announce-broadcast)) expose the relay path of a broadcast, which may reveal internal topology. A relay that does not wish to disclose its position MAY use the reserved value 0 ("unknown") instead of a stable identifier. The `Exclude Hop` filter in ANNOUNCE_REQUEST is a loop-avoidance hint, not an access control; a publisher is not required to honor it, and it MUST NOT be relied upon to hide broadcasts.

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
