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
  RFC1951:
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

When UDP is unavailable, moq-lite-05 MAY also run over reliable byte-stream transports via Qmux [qmux].
Qmux provides a length-delimited polyfill for QUIC streams on top of TCP/TLS or WebSocket; see [Transports](#transports) for the specific bindings and ALPN negotiation.

The session is active immediately after the QUIC/WebTransport connection is established.
Extensions are negotiated via stream probing: an endpoint opens a stream with an unknown type and the peer resets it if unsupported.

While moq-lite is a point-to-point protocol, it's intended to work end-to-end via relays.
Each client establishes a session with a CDN edge server, ideally the closest one.
Any broadcasts and subscriptions are transparently proxied by the CDN behind the scenes.

## Broadcast
A Broadcast is a collection of Tracks from a single publisher.
This corresponds to a MoqTransport's "track namespace".

A publisher may produce multiple broadcasts, each of which is advertised via an ANNOUNCE message.
The subscriber uses the ANNOUNCE_INTEREST message to discover available broadcasts.
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
- `Stale` (subscriber) indicates the maximum age before a non-latest Group is dropped from live delivery; `Cache` (publisher) indicates the minimum retention guarantee for FETCH and reconnects.

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

Each frame carries a presentation timestamp expressed in the parent Track's `Timescale` (units per second, negotiated in SUBSCRIBE_OK), and a duration in the same scale.
The timestamp is the source-of-truth for media time and is used by the moq-lite layer for [expiration](#expiration) decisions instead of wall-clock arrival time.
The duration is a hint for the application layer (e.g. presentation scheduling) and is not used by moq-lite itself; a duration of `0` means unknown and the frame is presented until the next frame begins.
A Track with a `Timescale` of 0 (unspecified) carries no meaningful timestamps or durations and falls back to wall-clock arrival time for expiration.

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
Bindings 3 and 4 are reliable byte-stream transports and have no datagram channel; a publisher MUST NOT emit datagrams on those bindings and MUST fall back to Group Streams.

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

### Announce
A subscriber can open an Announce Stream to discover broadcasts matching a prefix.

The subscriber creates the stream with an ANNOUNCE_INTEREST message.
The publisher replies with a single ANNOUNCE_OK message followed by ANNOUNCE messages for any matching broadcasts and any future changes.

ANNOUNCE_OK carries metadata that applies to every ANNOUNCE on this stream and is sent exactly once at the start of the response:

- The publisher's own `Hop ID`, which is the implicit trailing entry of every ANNOUNCE's Hop ID list. Hoisting it out of every ANNOUNCE saves bytes since it is identical for every announcement on the session.
- The number of `active` ANNOUNCE messages (`Active Count`) the publisher will send immediately as the initial set. The subscriber MAY buffer until all `Active Count` initial announcements arrive before reporting them to the application, avoiding a trickle. Any ANNOUNCE messages beyond `Active Count` are live updates and SHOULD be reported to the application as they arrive.

Each ANNOUNCE message contains one of the following statuses:

- `active`: a matching broadcast is available.
- `ended`: a previously `active` broadcast is no longer available.

Each broadcast starts as `ended`.
An `active` announcement makes the broadcast available; a subsequent `ended` makes it unavailable again.

A publisher SHOULD advertise only the best path it knows for each broadcast.
If the best path changes (e.g. a relay failover or upstream restart), the publisher MAY send another `active` for that broadcast: the new announcement atomically replaces the prior one (equivalent to UNANNOUNCE+ANNOUNCE).
A publisher MUST NOT keep multiple `active` advertisements for the same broadcast on the same stream — each broadcast has at most one current advertisement at a time.

The subscriber MUST reset the stream if it receives an `ended` for a broadcast that is not currently `active`, or any ANNOUNCE before ANNOUNCE_OK.
When the stream is closed, the subscriber MUST assume that all broadcasts are now `ended`.

Path prefix matching and equality is done on a byte-by-byte basis.
There MAY be multiple Announce Streams, potentially containing overlapping prefixes, that get their own ANNOUNCE_OK + ANNOUNCE messages.

### Subscribe
A subscriber opens Subscribe Streams to request a Track.

The subscriber MUST start a Subscribe Stream with a SUBSCRIBE message followed by any number of SUBSCRIBE_UPDATE messages.
The publisher replies with a SUBSCRIBE_OK message followed by any number of SUBSCRIBE_DROP and additional SUBSCRIBE_OK messages.
The first message on the response stream MUST be a SUBSCRIBE_OK; it is not valid to send a SUBSCRIBE_DROP before SUBSCRIBE_OK.

The publisher closes the stream (FIN) when every group from start to end has been accounted for, either via a GROUP stream (completed or reset) or a SUBSCRIBE_DROP message.
Unbounded subscriptions (no end group) stay open until the publisher closes the stream to indicate the track has ended, or either endpoint resets.
Either endpoint MAY reset/cancel the stream at any time.

### Fetch
A subscriber opens a Fetch Stream (0x3) to request a single Group from a Track.

The subscriber sends a FETCH message containing the broadcast path, track name, priority, and group sequence.
Unlike Group Streams (which MUST start with a GROUP message), the publisher responds with FRAME messages directly on the same bidirectional stream — there is no preceding GROUP header.
The Subscribe ID and Group Sequence for the returned FRAME messages are implicit, taken from the original FETCH request.
The publisher FINs the stream after the last frame, or resets the stream on error.

Fetch behaves like HTTP: a single request/response per stream.

### Probe
A subscriber opens a Probe Stream (0x4) to measure the available bitrate of the connection.

The subscriber sends a PROBE message with a target bitrate on the bidirectional stream.
The subscriber MAY send additional PROBE messages on the same stream to update the target bitrate; the publisher MUST treat each PROBE as a new target to attempt.
The publisher SHOULD pad the connection to achieve the most recent target bitrate.
The publisher periodically replies with PROBE messages on the same bidirectional stream containing the current estimated bitrate and smoothed RTT.

If the publisher does not support PROBE (e.g., congestion controller is not exposed), it MUST reset the stream.

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
The `Subscriber Priority` is scoped to the connection.
The `Publisher Priority` SHOULD be used to resolve conflicts or ties.

A conflict can occur when a relay tries to serve multiple downstream subscriptions from a single upstream subscription.
Any upstream subscription SHOULD use the publisher priority, not some combination of different subscriber priorities.

Rather than try to explain everything, here's an example:

**Example:**
There are two people in a conference call, Ali and Bob.

We subscribe to both of their audio tracks with priority 2 and video tracks with priority 1.
This will cause equal priority for `Ali` and `Bob` while prioritizing audio.
```
ali/audio + bob/audio: subscriber_priority=2 publisher_priority=2
ali/video + bob/video: subscriber_priority=1 publisher_priority=1
```

If Bob starts actively speaking, they can bump their publisher priority via a SUBSCRIBE_OK message.
This would cause tracks be delivered in this order:
```
bob/audio: subscriber_priority=2 publisher_priority=3
ali/audio: subscriber_priority=2 publisher_priority=2
bob/video: subscriber_priority=1 publisher_priority=2
ali/video: subscriber_priority=1 publisher_priority=1
```

The subscriber priority takes precedence, so we could override it if we decided to full-screen Ali's window:
```
ali/audio subscriber_priority=4 publisher_priority=2
ali/video subscriber_priority=3 publisher_priority=1
bob/audio subscriber_priority=2 publisher_priority=3
bob/video subscriber_priority=1 publisher_priority=2
```

### Ordered
The `Subscriber Ordered` field signals if older (0x1) or newer (0x0) groups should be transmitted first within a Track.
The `Publisher Ordered` field MAY likewise be used to resolve conflicts.

An application SHOULD use `ordered` when it wants to provide a VOD-like experience, preferring to buffer old groups rather than skip them.
An application SHOULD NOT use `ordered` when it wants to provide a live experience, preferring to skip old groups rather than buffer them.

Note that [expiration](#expiration) is not affected by `ordered`.
An old group may still be cancelled/skipped if it exceeds `max_latency` set by either peer.
An application MUST support gaps and out-of-order delivery even when `ordered` is true.


## Expiration
Expiration governs when an older group is dropped from a live subscription's Group Stream(s).
It is distinct from the publisher's retention guarantee (see `Publisher Cache` in [SUBSCRIBE_OK](#subscribe-ok)), which controls whether older groups remain available for FETCH or future subscriptions.

It is not crucial to aggressively expire groups thanks to [prioritization](#prioritization).
However, a lower priority group will still consume RAM, bandwidth, and potentially flow control.
It is RECOMMENDED that an application set conservative limits and only resort to expiration when data is absolutely no longer needed.

The publisher SHOULD reset Group Streams for non-latest groups whose age relative to the latest group exceeds the `Subscriber Stale` value in SUBSCRIBE/SUBSCRIBE_UPDATE.
The subscriber MAY also locally drop such groups for its own resource accounting.
Expiration only removes the group from the live subscription's stream; if the group's age is still within `Publisher Cache`, the publisher SHOULD retain it for FETCH or new subscriptions.

Group age is computed relative to the latest group by sequence number.
A group is never expired until at least the next group (by sequence number) has been received or queued.
Once a newer group exists, a group is considered expired if the time between its first frame and the latest group's first frame exceeds `Subscriber Stale`.

If the Track's negotiated `Timescale` is non-zero, the time delta is computed from per-frame timestamps (see [Frame](#frame)).
Otherwise the delta is computed from wall-clock arrival time: the first byte of a group received (subscriber) or queued (publisher).
Timestamp-based expiration is preferred because it remains consistent across relays and is unaffected by buffering or jitter.

A group that contains zero frames has no timestamp.
For expiration purposes its effective time is the wall-clock arrival/queue time of the group itself, regardless of the Track's `Timescale`.
This avoids stalling expiration on tracks that intentionally emit empty groups as keep-alives or gap markers.

An expired group SHOULD be reset at the QUIC level to avoid consuming flow control.

## Unidirectional Streams
Unidirectional streams are used for data transmission.

|--------|----------|-------------|
|     ID | Stream   | Creator     |
|-------:|:---------|-------------|
|    0x0 | Group    | Publisher   |
| ------ | -------- | ----------- |

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
  [Timestamp (i)]
  [Duration (i)]
  Payload (b)
}
~~~

`Timestamp` and `Duration` are present only when the Track's `Publisher Timescale` (see [SUBSCRIBE_OK](#subscribe-ok)) is non-zero.
When `Publisher Timescale` is 0, both fields are omitted from the wire and the datagram body consists of just `Subscribe ID`, `Group Sequence`, and `Payload`.

**Subscribe ID**:
The Subscribe ID of an active subscription on the same session.
A subscriber receiving a datagram with an unknown Subscribe ID MUST silently drop it.

**Group Sequence**:
The absolute sequence number of the group carried by this datagram.
Each datagram represents a complete group containing exactly one frame.

**Timestamp**:
The absolute timestamp of the single frame in the group, expressed in the Track's negotiated `Timescale`.
Any varint value (including 0) is a valid absolute timestamp.

**Duration**:
The absolute duration of the frame, expressed in the Track's negotiated `Timescale`.
A value of `0` means the duration is unknown; the frame is presented until the next frame begins (or indefinitely, since a datagram-delivered group contains exactly one frame, until the application supersedes it).

**Payload**:
The frame payload, extending to the end of the datagram.
If the Track's `Publisher Compression` is non-zero, the payload is compressed using the negotiated algorithm (see [SUBSCRIBE_OK](#subscribe-ok)).
The total datagram body (including all header fields above and the compressed payload if applicable) MUST NOT exceed 1200 bytes.
This limit ensures the datagram fits within the minimum QUIC path MTU without IP-layer fragmentation.
Payloads that would not fit MUST be sent as a Group Stream instead.
A receiver MUST silently drop any datagram that exceeds this limit.



# Encoding
This section covers the encoding of each message.

## Message Length
Most messages are prefixed with a variable-length integer indicating the number of bytes in the message payload that follows.
This length field does not include the length of the varint length itself.

An implementation SHOULD close the connection with a PROTOCOL_VIOLATION if it receives a message with an unexpected length.
The version and extensions should be used to support new fields, not the message length.

## STREAM_TYPE
All streams start with a short header indicating the stream type.

~~~
STREAM_TYPE {
  Stream Type (i)
}
~~~

The stream ID depends on if it's a bidirectional or unidirectional stream, as indicated in the Streams section.
A receiver MUST reset the stream if it receives an unknown stream type.
Unknown stream types MUST NOT be treated as fatal; this enables extension negotiation via stream probing.


## ANNOUNCE_INTEREST
A subscriber sends an ANNOUNCE_INTEREST message to indicate it wants to receive an ANNOUNCE message for any broadcasts with a path that starts with the requested prefix.

~~~
ANNOUNCE_INTEREST Message {
  Message Length (i)
  Broadcast Path Prefix (s),
  Exclude Hop (i),
}
~~~

**Broadcast Path Prefix**:
Indicate interest for any broadcasts with a path that starts with this prefix.

**Exclude Hop**:
If non-zero, the publisher SHOULD skip ANNOUNCE messages for broadcasts whose Hop ID entries (including the publisher's own `Hop ID` from ANNOUNCE_OK) contain this value.
This is used by relays to avoid routing loops in a cluster.

The publisher MUST respond with an ANNOUNCE_OK message followed by ANNOUNCE messages for any matching and active broadcasts, followed by ANNOUNCE messages for any future updates.
Implementations SHOULD consider reasonable limits on the number of matching broadcasts to prevent resource exhaustion.


## ANNOUNCE_OK
A publisher sends an ANNOUNCE_OK message exactly once, as the first message on the response side of an Announce Stream.
It carries metadata that is constant for the lifetime of the stream and applies to every ANNOUNCE that follows.

~~~
ANNOUNCE_OK Message {
  Message Length (i)
  Hop ID (i)
  Active Count (i)
}
~~~

**Hop ID**:
The publisher's own Hop ID.
This is treated as the implicit trailing entry of every ANNOUNCE's Hop ID list on this stream; ANNOUNCE messages MUST NOT repeat this value as the last entry of their `Hop ID` list.
A value of 0 indicates the publisher does not assign Hop IDs (e.g. when bridging from an older protocol version).
Receivers reconstruct the full path as `ANNOUNCE.Hop IDs ++ [ANNOUNCE_OK.Hop ID]`.

**Active Count**:
The number of `active` ANNOUNCE messages that the publisher will send immediately as the initial set.
The subscriber MAY block reporting any announcement to the application until all `Active Count` initial ANNOUNCEs have arrived, then deliver the initial set as a batch.
Any ANNOUNCE messages beyond `Active Count` are live updates and SHOULD be reported as they arrive.
A value of `0` is valid and means the publisher is offering no initial active broadcasts; all subsequent ANNOUNCEs (if any) are live updates.


## ANNOUNCE
A publisher sends an ANNOUNCE message to advertise a change in broadcast availability.
Only the suffix is encoded on the wire, as the full path can be constructed by prepending the requested prefix.

The status is relative to all prior ANNOUNCE messages for the same path on the same stream.
A publisher MAY send an `active` for a path that is already `active`: the new announcement atomically replaces the prior one, including any change to the Hop ID list.
An `ended` MUST follow a corresponding `active`; an `ended` for a path that is not currently `active` is a protocol violation.
An ANNOUNCE before ANNOUNCE_OK is a protocol violation.

~~~
ANNOUNCE Message {
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
The total path length is `Hop Count + 1` (including the implicit ANNOUNCE_OK `Hop ID`); this total is used as a tiebreaker when there are multiple paths to the same broadcast.
A Hop ID value of 0 indicates an unknown or bridged relay hop (e.g. when bridging from an older protocol version that does not assign Hop IDs); the Hop Count still reflects the total number of entries including unknown hops.


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
  Subscriber Stale (i)
  Start Group (i)
  End Group (i)
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

**Subscriber Stale**:
The subscriber's preference, in milliseconds, for how long a non-latest group may remain in flight before being considered stale and dropped from live delivery.
The publisher SHOULD reset (at the QUIC level) Group Streams for groups whose age relative to the latest group exceeds this duration.
Applies only to non-latest groups; the latest group is never dropped on staleness grounds.
A value of `0` means the subscriber wants only the latest group in live delivery (older groups are immediately stale once a newer group arrives).
This is a delivery-time preference, not a retention rule: the publisher's cache (see `Publisher Cache` in [SUBSCRIBE_OK](#subscribe-ok)) may still hold these groups for FETCH or future subscriptions.
See the [Expiration](#expiration) section for more information.

**Start Group**:
The first group to deliver.
A value of 0 means the latest group (default).
A non-zero value is the absolute group sequence + 1.

**End Group**:
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
  Subscriber Stale (i)
  Start Group (i)
  End Group (i)
}
~~~

See [SUBSCRIBE](#subscribe) for information about each field.


## SUBSCRIBE_OK {#subscribe-ok}
A SUBSCRIBE_OK message is sent in response to a SUBSCRIBE.
The publisher MAY send multiple SUBSCRIBE_OK messages to update the subscription.
The first message on the response stream MUST be a SUBSCRIBE_OK; a SUBSCRIBE_DROP MUST NOT precede it.

~~~
SUBSCRIBE_OK Message {
  Type (i) = 0x0
  Message Length (i)
  Publisher Priority (8)
  Publisher Ordered (8)
  Publisher Cache (i)
  Start Group (i)
  End Group (i)
  Publisher Timescale (i)
  Publisher Compression (i)
}
~~~

**Type**:
Set to 0x0 to indicate a SUBSCRIBE_OK message.

**Start Group**:
The resolved absolute start group sequence.
A value of 0 means the start group is not yet known; the publisher MUST send a subsequent SUBSCRIBE_OK with a resolved value.
A non-zero value is the absolute group sequence + 1.

**End Group**:
The resolved absolute end group sequence (inclusive).
A value of 0 means unbounded.
A non-zero value is the absolute group sequence + 1.

**Publisher Timescale**:
The number of timestamp units per second for frame timestamps on this Track.
A value of 0 means unspecified; the subscriber MUST treat per-frame timestamps as opaque and fall back to wall-clock arrival time for [expiration](#expiration).
When `Publisher Timescale` is 0, the per-frame `Timestamp Delta` and `Duration Delta` fields are omitted from FRAME messages and the `Timestamp` and `Duration` fields are omitted from datagram bodies (see [FRAME](#frame) and [Datagrams](#datagrams)).
A non-zero value is fixed for the lifetime of the subscription and MUST NOT change in subsequent SUBSCRIBE_OK messages; a change in timescale requires a new subscription.
Common values include `1000` (milliseconds), `1000000` (microseconds), `48000` (audio sample rate), and `90000` (RTP video clock).

**Publisher Cache**:
The minimum age, in milliseconds, the publisher guarantees to retain a group past the arrival of a newer group.
Applies only to non-latest groups; the latest group is always retained.
Analogous to HTTP `Cache-Control: max-age` as a lower bound:

- A subscriber MAY issue a SUBSCRIBE or FETCH with an older `Start Group` and expect the publisher to still have it, as long as the group's age does not exceed `Publisher Cache`.
- The publisher MAY retain groups longer than `Publisher Cache` (a best-effort cache beyond the guarantee); subscribers MUST NOT assume older groups are unavailable.

A value of `0` means no retention guarantee beyond live delivery; older groups MAY still be available but the publisher makes no promise.
The unit is milliseconds (independent of `Publisher Timescale`) so cache retention is decoupled from media time when timescale is unspecified.
The value MAY change in subsequent SUBSCRIBE_OK messages to reflect changing publisher policy; the subscriber SHOULD use the most recent value.

**Publisher Compression**:
The compression algorithm applied to every Frame `Payload` on this Track.

- `none` (0): payloads are transmitted verbatim (default).
- `deflate` (1): payloads are compressed with raw DEFLATE as defined in {{!RFC1951}}, with no zlib or gzip framing.

Compression is applied per-frame: each Frame `Payload` is an independent compressed stream with no shared dictionary or state between frames.
This keeps frames independently decodable and avoids head-of-line decoding within a group.
The Frame `Message Length` describes the compressed (on-wire) size.
An empty payload (size 0) MUST NOT be compressed and remains empty on the wire.

The publisher SHOULD only enable compression for payload types that benefit from it (e.g. JSON, text, uncompressed binary structures).
Already-compressed media (e.g. H.264, Opus, AV1) gains nothing and SHOULD use `none`.
The value is fixed for the lifetime of the subscription and MUST NOT change in subsequent SUBSCRIBE_OK messages; a change in compression requires a new subscription.
A subscriber that does not recognize the value MUST close the subscription with a protocol violation.

A relay MAY transcode payloads between compression algorithms (including bridging different protocol versions, e.g. a moq-lite-05 publisher to a moq-lite-04 subscriber) provided the decompressed bytes are identical to what the publisher produced.
A relay SHOULD NOT compress an originally-uncompressed payload unless there is a strong content signal that compression is beneficial (e.g. the track name ends in `.json`), because the relay cannot otherwise predict whether compression will help or hurt.

See [SUBSCRIBE](#subscribe) for information about the other fields.

## SUBSCRIBE_DROP
A SUBSCRIBE_DROP message is sent by the publisher on the Subscribe Stream when groups cannot be served.

~~~
SUBSCRIBE_DROP Message {
  Type (i) = 0x1
  Message Length (i)
  Start Group (i)
  End Group (i)
  Error Code (i)
}
~~~

**Type**:
Set to 0x1 to indicate a SUBSCRIBE_DROP message.

**Start Group**:
The first absolute group sequence in the dropped range.

**End Group**:
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

The publisher responds with FRAME messages on the same stream.
The publisher FINs the stream after the last frame, or resets on error.

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
When sent by the publisher (responder): the current estimated bitrate in bits per second.
A value of 0 means unknown.

**RTT**:
The smoothed round-trip time in milliseconds, as defined in {{!RFC9002}}.
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
  [Timestamp Delta (i)]
  [Duration Delta (i)]
  Message Length (i)
  Payload (b)
}
~~~

`Timestamp Delta` and `Duration Delta` are present only when the Track's `Publisher Timescale` (see [SUBSCRIBE_OK](#subscribe-ok)) is non-zero.
When `Publisher Timescale` is 0, both fields are omitted from the wire and the FRAME consists of just `Message Length` and `Payload`.

**Timestamp Delta**:
A signed delta from the previous frame's timestamp, in the Track's negotiated `Timescale`.
Encoded as a zigzag-mapped variable-length integer:

- Encode: `unsigned = (signed << 1) ^ (signed >> 63)` (arithmetic right shift).
- Decode: `signed = (unsigned >> 1) ^ -(unsigned & 1)`.

Zigzag interleaves non-negative and negative values (`0 → 0, -1 → 1, 1 → 2, -2 → 3, 2 → 4, ...`) so small magnitudes of either sign fit in a 1-byte varint and there is exactly one wire encoding for zero.
The first frame of a group is delta-encoded from `0`, so its `Timestamp Delta` is the zigzag encoding of the absolute timestamp.

**Duration Delta**:
A signed delta from the previous frame's duration, in the Track's negotiated `Timescale`, encoded using the same zigzag mapping as `Timestamp Delta`.
A wire value of `0` means the duration is unchanged from the previous frame; this is the common case for constant-rate media and fits in one byte.
The first frame of a group is delta-encoded from a prior duration of `0`.

The resolved duration value carries the following semantics for the application:

- A resolved duration of `0` means the duration is unknown; the frame is presented until the next frame in the group begins (or until the group ends, if it is the last frame).
- A non-zero resolved duration is the explicit presentation duration in the Track's `Timescale`.

The duration is an application-level hint and is not used by the moq-lite layer for delivery decisions.

**Payload**:
An application-specific payload.
If the Track's `Publisher Compression` is non-zero, the payload is compressed using the negotiated algorithm (see [SUBSCRIBE_OK](#subscribe-ok)) and the `Message Length` describes the compressed size.
A generic library or relay MUST NOT inspect or modify the decompressed contents unless otherwise negotiated; recompression that preserves the decompressed bytes exactly is allowed (see [SUBSCRIBE_OK](#subscribe-ok)).


# Appendix A: Changelog

## moq-lite-05
- Allowed a duplicate `active` ANNOUNCE to atomically replace the prior advertisement (equivalent to UNANNOUNCE+ANNOUNCE). Used when only the origin or hop path changes (e.g. relay failover) without interrupting the broadcast. No new wire enum value — the existing `active` status carries the new metadata.
- Added ANNOUNCE_OK message, sent once at the head of the Announce Stream response. Carries the publisher's `Hop ID` (hoisted out of every ANNOUNCE's Hop ID list) and an `Active Count` so subscribers can batch the initial set instead of reporting each ANNOUNCE as it trickles in.
- Added `Publisher Timescale` to SUBSCRIBE_OK for per-track timestamp negotiation. When `Publisher Timescale` is 0, the per-frame timestamp/duration fields are omitted entirely from FRAME and datagram bodies.
- Added `Timestamp Delta` and `Duration Delta` to FRAME, both zigzag-encoded signed varints (present only when timescale is non-zero). `Duration Delta = 0` is the common "unchanged" case and fits in one byte; a resolved duration of `0` means "until the next frame".
- Added `Timestamp` and `Duration` to the QUIC datagram body (absolute, present only when timescale is non-zero).
- Renamed `Publisher Max Latency` to `Publisher Cache` in SUBSCRIBE_OK, now defined as a minimum retention guarantee (similar to HTTP `Cache-Control: max-age`). Groups may live longer than `Publisher Cache` and remain FETCH-able.
- Renamed `Subscriber Max Latency` to `Subscriber Stale` in SUBSCRIBE/SUBSCRIBE_UPDATE. It is the subscriber's delivery-time preference for dropping non-latest stale groups, separate from the publisher's retention guarantee.
- Timestamp-based expiration replaces wall-clock arrival time when a Track timescale is negotiated.
- Added QUIC datagram delivery for groups, sharing Subscribe IDs with existing subscriptions (no separate control stream).
- Added `Publisher Compression` to SUBSCRIBE_OK for per-frame payload compression (`none` or `deflate`).
- Added Qmux [qmux] transport bindings for TCP/TLS and WebSocket, for environments where UDP is unavailable. The WebSocket binding uses the WebSocket message framing in place of the Qmux Record `Size` field.

## moq-lite-04
- Renamed ANNOUNCE_PLEASE to ANNOUNCE_INTEREST.
- ANNOUNCE `Hops` count replaced with explicit `Hop ID` list for loop detection.
- Added `Exclude Hop` to ANNOUNCE_INTEREST for relay loop avoidance.
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
- Added `Hops` to ANNOUNCE.
- Added `Subscriber Stale` and `Subscriber Ordered` to SUBSCRIBE and SUBSCRIBE_UPDATE.
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
- Extensions negotiated via stream probing instead of parameters.
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
- SUBSCRIBE_NAMESPACE -> ANNOUNCE_INTEREST
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
TODO Security


# IANA Considerations

This document has no IANA actions.


--- back

# Acknowledgments
{:numbered="false"}

TODO acknowledge.
