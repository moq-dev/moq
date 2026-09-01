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

A publisher advertises what it can serve via ANNOUNCE_START messages, each carrying a path prefix: a route covering every broadcast path beneath it.
The subscriber uses the ANNOUNCE_REQUEST message to discover these routes, then subscribes to specific paths under them; which paths name broadcasts is an application convention.
The common convention is that a publisher announces each broadcast's exact path as its own route, so subscribers can enumerate broadcasts, while a service announces one short prefix and serves whatever is requested beneath it.
Announcements are live and can change over time, allowing for dynamic origin discovery.

A broadcast consists of any number of Tracks.
The contents, relationships, and encoding of tracks are determined by the application.

## Track
A Track is a series of Groups identified by a unique name within a Broadcast.

A track consists of a single active Group at any moment, called the "latest group".
When a new Group is started, the previous Group is closed and may be dropped for any reason.
The duration before an incomplete group is dropped is determined by the application and the publisher/subscriber's latency target.

Every subscription is scoped to a single Track.
A subscription starts at a configurable Group (defaulting to the latest) and continues until a configurable end Group or until either the publisher or subscriber cancels the subscription.
Both bounds may be refined to a Frame within their Group, so a subscription can start or stop partway through a Group rather than only on a Group boundary (see [Positions](#positions)).

The subscriber and publisher both indicate their delivery preference:
- `Priority` indicates if Track A should be transmitted instead of Track B.
- `Subscriber Max Age` indicates the maximum age before a non-latest Group is dropped from live delivery; `Publisher Max Age` indicates the maximum age before a non-latest Group is dropped from the publisher's cache.

The combination of these preferences enables the most important content to arrive during network degradation while still respecting encoding dependencies.

## Group
A Group is an ordered stream of Frames within a Track.

Each group consists of an append-only list of Frames.
A Group is normally served by a dedicated QUIC stream which is closed on completion, reset by the publisher, or cancelled by the subscriber.
This ensures that all Frames within a Group arrive reliably and in order.

In contrast, Groups may arrive out of order due to network congestion and prioritization.
The application SHOULD process or buffer groups out of order to avoid blocking on flow control.

A small single-frame Group MAY instead be transmitted as a QUIC datagram when reliability is not required (see [Datagrams](#datagrams)).

## Frame
A Frame is a payload of bytes within a Group.

A frame is used to represent a chunk of data with an upfront size.
The contents are opaque to the moq-lite layer.

Frames within a Group are numbered from 0 in the order they were produced.
This index is not transmitted per frame; it is implied by position within the Group, anchored by the [GROUP](#group) message's `Frame Start`.
A Group Stream normally starts at frame 0, but MAY start later when the publisher only holds (or was only asked for) part of the Group.

Each frame carries a presentation timestamp expressed in the parent Track's `Timescale` (see [TRACK_INFO](#track-info)), used by the moq-lite layer for [expiration](#expiration) decisions.

## Positions {#positions}
A Position is a (Group Sequence, Frame Index) pair identifying one frame within a Track.
Positions order lexicographically: by group first, then by frame within the group.

SUBSCRIBE and FETCH bound their delivery by Position rather than by Group alone.
Each carries a `Frame Start` qualifying its start group and a `Frame End` qualifying its end group; both default to the whole group, which is the behavior of a draft that has no such field.

The bounds are chosen so that two subscriptions abut exactly.
`Frame Start` is the index of the first frame to deliver and `Frame End` is the index of the last (inclusive), so a subscriber that has received frames 0 through `N-1` of group `G` and wants the remainder asks for `Group Start` = `G`, `Frame Start` = `N`.
Capping the first subscription at `Group End` = `G`, `Frame End` = `N-1` covers exactly the complement with no gap and no overlap.
Because `Group End` and `Frame End` are both encoded as `absolute + 1` (see [SUBSCRIBE](#subscribe)) while `Group Start` and `Frame Start` are not, the two requests carry the *same* numbers on the wire; the end bound is effectively exclusive once encoded.

This is what lets a subscriber move a Track to a different publisher partway through a Group instead of waiting for the next Group to start, which matters for Tracks whose current Group may stay open indefinitely.
A Group can therefore be assembled from more than one publisher: each contributes a disjoint run of frames, and the subscriber concatenates them in index order.

A partial Group is only ever delivered to a subscriber that asked for one.
A publisher that cannot serve a Group from the frame the subscription names MUST skip that Group entirely and resolve the subscription to a later one, rather than deliver the part it holds.
The resolved start is therefore always either exactly the requested Position or the beginning of a later Group, never some third Position the subscriber did not choose.

The asymmetry with Groups is deliberate.
A Group is the unit of decodability: dropping leading *Groups* leaves a stream the application can still decode, whereas dropping leading *Frames* often does not, whether because the Group opens with a keyframe the rest depends on or because its compression state lives in the frames that went missing.
Only the subscriber knows whether a partial Group is any use to it, so only the subscriber may ask for one.

Frame indices are only meaningful relative to a Group the subscriber has already begun receiving, so:

- A `Frame Start` qualifies the group `Group Start` names, group 0 included.
  A subscriber that has seen no such group MUST send `Frame Start` = 0, since it cannot number the frames of a group it has not received.
- A `Frame End` is meaningless without a `Group End`; an unbounded subscription (`Group End` = 0) MUST send `Frame End` = 0.

A publisher MUST treat a violation of either rule as a protocol violation and reset the stream.

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

## Error Codes {#error-codes}
There are two independent error code spaces, one for terminating the session and one for resetting a stream.
The same numeric value means different things in each, so an endpoint MUST select the code from the space matching what it is terminating.

Both spaces reuse the codes moq-transport assigns, unchanged and with the same meaning, so an endpoint that speaks both protocols has one vocabulary and a relay can forward a peer's code without translating it.
The codes moq-lite uses are listed in full below; an endpoint MUST NOT assign a moq-lite specific meaning to any code below 32.

Codes 64 and above are the application's, opaque to moq-lite.
An endpoint MUST ignore a code it does not recognize, treating it as an unspecified error.
An endpoint MUST NOT infer a meaning for an unregistered code; in particular, it MUST NOT assume a code is an authorization failure unless it is UNAUTHORIZED.

Codes 32 through 63 are reserved and MUST NOT be interpreted.
Implementations currently emit values in this range for conditions with no code above, but those values are provisional placeholders, not assignments: a receiver MUST treat one as an unspecified error, exactly as it would any other unregistered code.
A future revision will assign this range, or fold the conditions into the shared codes.

### Session Error Codes
Sent when terminating the session, via the transport's session close.

| Code | Name | Description |
| ------- | ------------- | ----------- |
|  0x0   | NO_ERROR | The session was terminated without error. |
| ------- | ------------- | ----------- |
|  0x1   | INTERNAL_ERROR | An implementation-specific error. |
| ------- | ------------- | ----------- |
|  0x2   | UNAUTHORIZED | The endpoint is not authorized to establish the session, or to perform an operation on it. |
| ------- | ------------- | ----------- |
|  0x3   | PROTOCOL_VIOLATION | The peer violated this specification. |
| ------- | ------------- | ----------- |
|  0x6   | KEY_VALUE_FORMATTING_ERROR | A key-value pair was malformed, or repeated more than allowed. |
| ------- | ------------- | ----------- |
|  0x10  | GOAWAY_TIMEOUT | The peer did not close within the GOAWAY drain deadline. |
| ------- | ------------- | ----------- |
|  0x11  | CONTROL_MESSAGE_TIMEOUT | The peer took too long to respond to a control message. |
| ------- | ------------- | ----------- |
|  0x15  | VERSION_NEGOTIATION_FAILED | No version could be negotiated. |
| ------- | ------------- | ----------- |

### Stream Error Codes
Sent when resetting a stream (RESET_STREAM), or when refusing to receive one (STOP_SENDING).

| Code | Name | Description |
| ------- | ------------- | ----------- |
|  0x0   | INTERNAL_ERROR | An implementation-specific error. |
| ------- | ------------- | ----------- |
|  0x1   | CANCELLED | The stream was cancelled by either endpoint. A routine unsubscribe. |
| ------- | ------------- | ----------- |
|  0x2   | DELIVERY_TIMEOUT | The content missed its delivery deadline. |
| ------- | ------------- | ----------- |
|  0x3   | SESSION_CLOSED | The session is closing, taking this stream with it. |
| ------- | ------------- | ----------- |
|  0x4   | GOING_AWAY | A GOAWAY was sent or received. |
| ------- | ------------- | ----------- |
|  0x5   | TOO_FAR_BEHIND | The reader fell too far behind and content was dropped to catch up. |
| ------- | ------------- | ----------- |
|  0x12  | MALFORMED_TRACK | The track's content could not be parsed. |
| ------- | ------------- | ----------- |

Note that CANCELLED is 0x1, not 0x0: a stream reset with 0x0 is an INTERNAL_ERROR, not a routine cancellation.
An endpoint terminating a stream because the session is ending SHOULD use SESSION_CLOSED rather than the session's own code, since the two spaces are disjoint.

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
A subscriber can open an Announce Stream to discover routes matching a prefix.
A route is an advertisement that paths under its prefix can be served; it claims capability, not inventory, so it never asserts that any specific broadcast exists.

The subscriber creates the stream with an ANNOUNCE_REQUEST message.
The publisher replies with a single ANNOUNCE_OK message followed by announcements for any matching routes and any future changes:

- ANNOUNCE_START: a matching route is available.
- ANNOUNCE_END: a previously started route is no longer advertised.
- ANNOUNCE_UPDATE: a previously started advertisement was atomically updated (new hops or cost).

ANNOUNCE_OK carries metadata that applies to every announcement on the stream: the publisher's own `Hop ID` (the implicit trailing entry of every announcement's path) and the number of initial announcements, which lets the subscriber deliver the initial set as a batch (see [ANNOUNCE_OK](#announce-ok)).

Each ANNOUNCE_START implicitly assigns the next Announce ID on the stream: a counter starting at 0 that increments by 1 per ANNOUNCE_START.
The id never appears on the wire; both endpoints derive it from the message order on the (reliable, ordered) stream.
ANNOUNCE_END and ANNOUNCE_UPDATE reference the Announce ID instead of repeating the route's prefix.

Each route prefix has at most one current advertisement per stream.
A second ANNOUNCE_START for an already-advertised prefix is a protocol violation; an ANNOUNCE_UPDATE atomically updates the current advertisement's metadata while keeping its id live.

The subscriber MUST close the session with a PROTOCOL_VIOLATION if it receives an ANNOUNCE_END or ANNOUNCE_UPDATE referencing an Announce ID that was never assigned or already retired, an ANNOUNCE_START for a prefix that is already advertised, or any announcement before ANNOUNCE_OK.
When the stream is closed, the subscriber MUST assume that all routes are now unavailable.

A route covers a path when its prefix is a leading run of the path's segments; matching is per path segment, so a prefix never matches half a segment, and equality is byte-by-byte within each segment.
A publisher answering a request stream presents each of its routes clamped to the intersection with the requested prefix: a route above the request's prefix appears as the request prefix itself (an empty suffix), which is exactly the covered set the subscriber may see.
There MAY be multiple Announce Streams, potentially containing overlapping prefixes, that get their own ANNOUNCE_OK + announcements.

#### Routing {#routing}
Each advertisement carries the path of Hop IDs it traversed and an accumulated Warm and Cold Route Cost (see [ANNOUNCE_START](#announce-start)), which relays use to build a loop-free mesh.

A receiver MUST discard an announcement whose reconstructed path contains its own Hop ID: it has looped back, so forwarding it would extend the loop and subscribing through it would route the receiver back to itself.
This is the only loop defense moq-lite requires, and it catches loops of any length.
A conforming sender never sends one (see below), so a receiver MAY instead close the session with a protocol violation; discarding is what keeps a mesh working when one member does not conform.
A Hop ID of 0 means unknown and never matches anything; withholding an ID trades loop detection for privacy.

A publisher MUST NOT advertise a path whose entries contain the Hop ID the subscriber declared in its SETUP (see [Hop Parameter](#hop-parameter)).
The receiver can only discard it, and acting on it would form a loop, so sending one is never useful.
Of the paths that remain a publisher SHOULD advertise the best, and nothing when every known path contains that Hop ID.
Selection is per subscriber, so a subscriber that the serving path flows through still receives the best standby path, which is what lets it fail over if its own copy dies.
The per-subscriber winner changing travels as an ANNOUNCE_UPDATE; the last qualifying path appearing or disappearing travels as an ANNOUNCE_START or ANNOUNCE_END.

When serving a subscription, a publisher MUST select the source by that same exclusion; if only excluded sources remain, the subscription is unroutable.
Applying one rule to both advertisement and dispatch keeps advertised paths truthful, which is what prevents subscription cycles of any length.

When resolving a path covered by several routes (across any number of streams), the subscriber SHOULD prefer the most specific covering prefix, then the lowest Warm Route Cost after adding each arriving link's cost (see [Cost Parameter](#cost-parameter)), breaking ties toward the lowest Cold Route Cost, then toward the shortest path, and then toward the most recently received, so a reconnecting publisher is not outranked by the stale session it replaced.
The Cold tie-break matters exactly where the Warm one runs out: two relays that both carry the content both advertise a Warm cost of 0, and only their Cold costs say which of them sits closer to the publisher.

A route carries no content identity: nothing on the wire promises that two routes covering one path serve interchangeable bytes.
A relay MUST NOT splice a live subscription across sources reached through different routes; when a serving session ends, in-flight subscriptions end with it (a reset), and the subscriber re-requests through the best remaining route.

### Subscribe
A subscriber opens Subscribe Streams to request a Track.

The subscriber MUST start a Subscribe Stream with a SUBSCRIBE message followed by any number of SUBSCRIBE_UPDATE messages.
The publisher replies with a SUBSCRIBE_OK message once the start group is resolved, followed by any number of SUBSCRIBE_END and SUBSCRIBE_DROP messages.
For a live track the publisher MAY withhold SUBSCRIBE_OK until the first matching group resolves the start; if the track has already ended with no matching groups, it sends SUBSCRIBE_END with no preceding SUBSCRIBE_OK.
A rejection is a stream reset: a publisher that cannot serve the subscription (no such track, an ended broadcast, or any other refusal) MUST promptly reset the stream rather than leave it pending, so a subscriber distinguishes "pending" from "refused" by the reset, not by a timeout.
A route claims capability rather than inventory, so a subscription for a covered path that names nothing is refused this way too.

The track's immutable publisher properties are not carried here; they are fetched once via a [Track Stream](#track-stream).
The subscriber needs the track's TRACK_INFO (notably its timescale) to interpret FRAME messages, and MAY open the Track and Subscribe streams concurrently, buffering frames until it arrives.

The publisher sends SUBSCRIBE_OK once the absolute start position is resolved, and SUBSCRIBE_END once no further groups will be produced (see [SUBSCRIBE_OK](#subscribe-ok) and [SUBSCRIBE_END](#subscribe-end)).
The publisher closes the stream (FIN) only once every group from start to end has been accounted for, either via a Group Stream (completed or reset) or a SUBSCRIBE_DROP message.
This MAY occur after SUBSCRIBE_END, since stragglers within the range can still be dropped.
Unbounded subscriptions stay open until SUBSCRIBE_END, and either endpoint MAY reset the stream at any time.

### Fetch
A subscriber opens a Fetch Stream (0x3) to request a single Group from a Track.

The subscriber sends a FETCH message containing the broadcast path, track name, priority, group sequence, and the frame range within that group.
Unlike SUBSCRIBE, FETCH works on both live and ended broadcasts; it is the only way to read an ended one.
The publisher responds with FRAME messages directly on the same bidirectional stream — there is no response header.
The Subscribe ID, Group Sequence, and index of the first returned frame are implicit, taken from the original FETCH request.
Because there is no response header, a publisher that cannot serve the requested frame range in full MUST reset the stream rather than return a shorter run; the subscriber has no way to learn where a truncated response actually started.
As with a subscription, the subscriber MUST already have the track's [TRACK_INFO](#track-info) to parse the returned frames; because the properties are immutable, a single Track Stream lookup is reused across every FETCH of that track (group-by-group fetches do not re-fetch it).
The publisher FINs the stream after the last frame, or resets the stream on error.

Fetch behaves like HTTP: a single request/response per stream.

### Track {#track-stream}
A subscriber opens a Track Stream (0x6) to learn a Track's immutable publisher properties without subscribing or fetching.

The subscriber sends a TRACK message containing the broadcast path and track name.
The publisher replies with a single TRACK_INFO message and then FINs the stream, or resets the stream on error (e.g. the track does not exist).
The returned properties are fixed for the lifetime of the track, so the subscriber SHOULD cache TRACK_INFO keyed by broadcast path and track name, and reuse it across every SUBSCRIBE and FETCH of the same track over the session that served it.
If FRAME messages cannot be decoded against the cached TRACK_INFO, the subscriber MUST reset the affected stream with a protocol violation and re-request it.

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
The Publisher and Subscriber both exchange a `Priority` value, which determines which Track should be transmitted next.
Group order within a Track is fixed: newest first.

A publisher SHOULD attempt to transmit streams based on these rules.
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

### Group Order
Within a Track, a publisher SHOULD transmit the newest Group first.
Under congestion this sheds the backlog rather than the live edge, which is what a Group boundary exists to make possible.

There is no field to invert it.
A subscriber that wants Groups in sequence order gets them by reading in sequence order: reordering is a local decision, it costs the network nothing, and encoding it on the wire only lets one subscriber's preference reach a Track that a relay is fanning out to many.
Such a subscriber SHOULD raise its `Subscriber Max Age` so the Groups it intends to read in order are still being delivered when it reaches them.

A subscriber MUST support gaps and out-of-order delivery regardless.


## Expiration
Expiration governs when an older group is dropped.
The publisher SHOULD reset Group Streams for non-latest groups whose age relative to the latest group reaches `Subscriber Max Age` (see [SUBSCRIBE](#subscribe), and the age definition below); the subscriber MAY also locally drop such groups.
Expiration only removes the group from live delivery; the publisher MAY still retain it for FETCH or new subscriptions until its age exceeds `Publisher Max Age` (see [TRACK_INFO](#track-info)).

It is not crucial to aggressively expire groups thanks to [prioritization](#prioritization), but a lower priority group still consumes RAM, bandwidth, and potentially flow control.
It is RECOMMENDED that an application set conservative limits and only resort to expiration when data is absolutely no longer needed.

A group is never expired until a later group (by sequence number) has presented a frame.
Once one has, the group's **timestamp age** is the difference between the *newest* frame timestamp of the latest group that has at least one frame, and this group's **reach**, defined below; the group is expired once that age meets the relevant `Max Age`.
Timestamps are the only measure: they are consistent across relays and unaffected by buffering or jitter.
Wall-clock reclamation of idle content is the retention cache's own policy (`Publisher Max Age`, or any implementation-defined bound), not part of this rule.

A group is expired only once it **provably cannot overlap the Max Age window**: every frame it could still present is older than the budget allows.
Being behind is not itself a reason to expire anything.
[Prioritization](#prioritization) already transmits newer groups first, so an older group consumes only whatever capacity is left over; it therefore closes the gap faster than the live edge advances, and a receiver that is behind can converge without losing content.
What cannot be recovered is a group with nothing left worth delivering, and that is what expiration removes.

A group's **reach** is the first frame timestamp of the next group by sequence number, or unbounded when no later group has presented a frame.
A group cannot present past where its successor begins, so this is the furthest it could still reach.
Its own frames do not bound it: frame durations are not carried on the wire, so a group's last frame timestamp is where that frame *starts* presenting, not where the group ends.
A group whose successor has not presented a frame is therefore never expired, because nothing yet proves where it stops.
The group itself needs no timestamp: a zero-frame group (a keep-alive or gap marker) is bounded by its successor's first frame the same way.

Reach is an exclusive bound, so a timestamp age **equal** to Max Age already expires the group: the freshest frame it could still hold sits strictly below its reach, and is therefore strictly older than the budget.
A Max Age of zero follows from the same rule without a special case.

Measuring in timestamps rather than arrival means a burst of groups delivered together still reads as its true age, so catching up never resets the clock.

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

The GROUP message's `Frame Start` gives the index of the first FRAME on the stream, so a stream carrying only part of a Group is self-describing.
It is redundant with the subscription's own `Frame Start` in the steady state, and carried anyway because a SUBSCRIBE_UPDATE that moves the bound races the Group Streams already in flight: the two travel on different streams with no ordering between them, so the receiver would otherwise have to guess which bound a given Group Stream was opened under.

A publisher MUST NOT send two Group Streams for the same Group Sequence on one subscription.
A subscriber assembling a Group from more than one publisher does so across separate subscriptions, and is responsible for concatenating the runs in index order and for ignoring any frame it has already received.

Both the publisher and subscriber MAY reset the stream at any time.
This is not a fatal error and the session remains active.
The subscriber MAY cache the error and potentially retry later.

## Datagrams
QUIC datagrams provide unreliable, unordered delivery for latency-sensitive content that does not need retransmission.

A publisher MAY transmit a Group consisting of exactly one Frame as a single QUIC datagram, in addition to (or instead of) opening a Group Stream, based on application hints, group size, and network conditions; a multi-frame Group is delivered via a Group Stream only.
A datagram-delivered group is not cached or retransmitted; a publisher SHOULD only send a datagram if the congestion controller can transmit it immediately.
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
| 0x5  | Hop       | Hop ID (i)  |
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
The Cost Parameter declares what subscribing from this endpoint costs: a receiver adds the value the sender declared to both Route Costs of every announcement that sender forwards (see [Routing](#routing)).

The Parameter Value is a variable-length integer in deployment-chosen units, the same units as the Route Costs.
An absent parameter means the default cost of 1, under which the accumulated Route Costs equal the hop count and routing degenerates to shortest-path.
A value of 0 is meaningful and distinct from absent: it makes that direction free, e.g. between two relays in the same datacenter.

Both endpoints send it and the two values need not match: the parameter prices the sender's own egress. A relay MUST NOT forward it.

A declared cost is an assertion, not an instruction: a receiver MAY charge a locally configured value instead, so a peer cannot reprice its neighbours by declaring itself cheap.

### Hop Parameter {#hop-parameter}
The Hop Parameter declares the sender's Hop ID: the identity it stamps onto announcements it forwards.
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

The publisher MUST respond with an ANNOUNCE_OK message followed by ANNOUNCE_START messages for any matching and available broadcasts, followed by ANNOUNCE_START, ANNOUNCE_END, and ANNOUNCE_UPDATE messages for any future updates, subject to [Routing](#routing).
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
This is treated as the implicit trailing entry of every ANNOUNCE_START and ANNOUNCE_UPDATE Hop ID list on this stream; those messages MUST NOT repeat this value as the last entry of their `Hop ID` list.
The value 0 is reserved to mean "unknown": either no Hop ID was assigned (e.g. when bridging from an older protocol version) or the endpoint deliberately withholds it to obscure the underlying routing.
A publisher that assigns a Hop ID MUST choose a non-zero value, and SHOULD assign itself one (a fresh random value per session suffices), so downstream receivers can detect loops through it.
Receivers reconstruct the full path as `Hop IDs ++ [ANNOUNCE_OK.Hop ID]`.

**Active Count**:
The number of ANNOUNCE_START messages that the publisher will send immediately as the initial set.
The subscriber MAY block reporting any announcement to the application until all `Active Count` initial announcements have arrived, then deliver the initial set as a batch.
Any announcements beyond `Active Count` are live updates and SHOULD be reported as they arrive.
A value of `0` is valid and means the publisher is offering no initial available broadcasts; all subsequent announcements (if any) are live updates.


## ANNOUNCE_START {#announce-start}
A publisher sends an ANNOUNCE_START message to advertise a route: a claim that paths under a prefix can be served.
Each ANNOUNCE_START implicitly assigns the next Announce ID on the stream, later referenced by ANNOUNCE_END and ANNOUNCE_UPDATE (see [Announce](#announce)).

Only the suffix is encoded on the wire, as the full route prefix can be constructed by prepending the requested prefix.

~~~
ANNOUNCE_START Message {
  Type (i) = 0x0
  Message Length (i)
  Route Prefix Suffix (s),
  Hop Count (i),
  Hop ID (i) ...,
  Warm Route Cost (i),
  Cold Route Cost (i),
}
~~~

**Type**:
Set to 0x0 to indicate an ANNOUNCE_START message.

**Route Prefix Suffix**:
This is combined with the requested prefix to form the route's full prefix.
An empty suffix advertises the requested prefix itself, which is how a route covering more than the request presents (see [Announce](#announce)).

**Hop Count**:
The number of Hop ID entries that follow, NOT including the publisher's own `Hop ID` from ANNOUNCE_OK.
A value of 0 means no Hop ID entries are present, indicating either that the announcement originated locally on the publisher (the publisher itself is the origin) or that the upstream peer does not support hop tracking.
A receiver MUST close the stream with a PROTOCOL_VIOLATION if the Hop Count does not match the number of subsequent Hop ID entries.

**Hop ID**:
A unique identifier for each relay in the path from the origin publisher, ordered from origin to the upstream of the responding publisher.
The responding publisher's own Hop ID is NOT included in this list; it is carried once in ANNOUNCE_OK, so the total path length is `Hop Count + 1`.
When forwarding an announcement received from an upstream peer, a relay MUST append the upstream peer's ANNOUNCE_OK `Hop ID` to this list, since that ID is no longer implicit downstream.
The first entry of the reconstructed path identifies the endpoint that originated the route.
A Hop ID value of 0 means the hop is unknown: either it was never assigned or a relay deliberately withholds it (see [Routing](#routing)).

A receiver MUST close the session with a PROTOCOL_VIOLATION if a non-zero Hop ID appears twice in this list.
Duplicate values of 0 are not a violation, since 0 identifies nothing and any number of hops may be unknown.

**Warm Route Cost** and **Cold Route Cost**:
What subscribing to content under this route costs, in units chosen by the deployment, priced against two different cache states.
The Warm cost is what one more subscription would cost the mesh as it stands, and is what routing minimizes.
The Cold cost prices the identical path as if no relay along it were carrying anything, and exists to rank two relays that both discounted their Warm cost to 0.
The original publisher seeds both with its production cost: 0 for content it is already producing, larger for content it would have to start producing on demand (e.g. a standby transcoder that advertises every broadcast it could serve, at a cost reflecting the work of actually serving it).
When forwarding an announcement received from an upstream peer, a relay adds the cost that peer declared (see [Cost Parameter](#cost-parameter)) to both, saturating rather than wrapping so an absurd upstream value ranks last instead of overflowing to best.
Saturation MUST cap each sum at the largest value a variable-length integer can carry, since the sums are re-encoded when forwarded: a peer may legally advertise that largest value, and a wider ceiling would leave the relay unable to encode what it just computed.

A relay that is actively carrying content under the route (a live subscription exists through it) MAY advertise a Warm cost of 0 instead of the accumulated value: its ingress is already paid for, which is what lets a cluster deduplicate onto a warm copy.
The discount applies to the Warm cost only; the Cold cost is forwarded accumulated, since it prices the path the relay would have to open if it were not already carrying it.
When the relay stops carrying content under the route it SHOULD restore the accumulated value via ANNOUNCE_UPDATE, optionally after a grace period so brief churn does not flap routing.

A relay whose wire cannot express a Cold cost (an endpoint bridging from another protocol, or a peer that predates this field) advertises nothing, and a receiver SHOULD treat the missing value as the saturation ceiling rather than as 0: an unknown path ranks last instead of impersonating the publisher's own.

A carrying relay whose serving path costs the saturation ceiling SHOULD forgo the discount and advertise the ceiling instead.
Draining is the ceiling's primary producer: a session that received a GOAWAY (see [GOAWAY](#goaway-message)) prices its routes there, since the ingress the discount priced in is going away and a zero-cost advertisement would keep attracting subscribers to a path that a subscriber with any alternative should leave while the handover window is open.
The rule is deliberately keyed on the value rather than on why it was reached: a cost that saturated through accumulated charges marks a path of last resort all the same, and value-keyed behavior is what independent implementations can agree on, since the reason does not travel on the wire.
Forgoing the discount is also what carries a drain across a mesh: each carrying relay along the path repeats it, so the ceiling survives hops that would otherwise re-mask it as 0.

Two relays that independently begin carrying the same content would each see the other's 0 as cheaper than its own source, and both switching at once would leave the content with no source.
Before re-parenting onto a 0-cost advertisement from another actively-carrying relay (one whose path has two or more entries), a relay SHOULD require that relay's rank to be strictly lower than its own, where a relay's rank is the Cold Route Cost of the path it serves from, followed by a hash of the route prefix and its Hop ID.
Adopting a parent adds that link's cost to the adopting relay's own Cold cost, so once the move lands it ranks above the relay it adopted, and the two cannot adopt each other.
Ranking on Cold cost first also puts the aggregation point at the relay with the cheapest path to the publisher, instead of wherever a hash happens to fall; the hash only separates relays that are equally far from it.
Equal ranks (including two relays that both declared Hop ID 0) cannot be ordered, and neither side SHOULD move.
Cheaper advertisements from anything else carry no such hazard and MAY be adopted immediately.

This ordering is only shared between two relays while the costs behind it are.
A relay reports its own Cold Route Cost, and a report still crossing the mesh can be lower than the value its sender would report now, so while costs are rising three or more relays can each rank a stale neighbour below themselves and all re-parent at once, leaving the broadcast with no source until the real advertisements arrive.
A relay SHOULD therefore delay re-parenting onto a route learned from a peer, re-evaluating when the delay expires rather than acting on the decision that started it, so the advertisements the decision rests on are refreshed before it takes effect.
The delay MUST exceed the time an advertisement takes to cross the mesh, and SHOULD carry a spread that is stable per relay and broadcast, so that a group of relays reconsidering the same broadcast does not do so on a single instant.
Having no subscribers does not exempt a relay: the choice it records is the one it will pull down when a subscriber does arrive, so an unserved relay is a ring that starts later rather than one that cannot form.
Neither does a short path, since a peer that does not carry Hop IDs is indistinguishable from the route's origin however deep the chain behind it is.
Two cases are exempt: a fresh session from the peer already being pulled from, which is the same dependency rather than a new one, and a route that has disappeared, since waiting strands the relay with nothing to serve from at all.

The second exemption is a deliberate residual rather than a proof.
Relays that lose their routes together can each re-parent onto a neighbour whose advertised cost has not yet caught up, forming the same ring the delay exists to prevent.
It is accepted because the alternative is worse in the common case: an isolated relay that waits has no source for the length of the delay, isolated losses far outnumber correlated ones, and a ring that does form is broken by the hop chain once the real paths propagate.

A draining route is deliberately not exempt, even though leaving one is urgent.
A session that received a GOAWAY keeps serving until its handover window closes, so the delay costs a relay some optimality rather than any availability, while a fleet draining together is exactly the case where several relays re-parent at once off costs that have not yet propagated.
The exemptions also compose: should the drain become a disconnection, the route leaves the relay's table and the disappeared-route case applies immediately, so the delay is bounded by the session it is waiting on.


## ANNOUNCE_END {#announce-end}
A publisher sends an ANNOUNCE_END message to retract a previously started route, referencing its Announce ID.
The id is retired and MUST NOT be referenced again.
Retraction claims nothing about the content: a broadcast whose publisher stops advertising may remain readable by exact path, serving its stored groups over FETCH.
Retraction does not disturb subscriptions already in flight, which conclude normally with SUBSCRIBE_END.
How a subscriber learns the path of stored content is out of band, e.g. an application catalog.

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
Announce IDs are never reused within a stream; a prefix that is announced again after an ANNOUNCE_END gets a fresh id from its next ANNOUNCE_START.


## ANNOUNCE_UPDATE {#announce-update}
A publisher sends an ANNOUNCE_UPDATE message to atomically update a previously started advertisement's metadata, referencing its Announce ID.
The route's prefix is unchanged and the id stays live; the Hop ID list MAY differ from the original (e.g. after a relay failover or upstream restart).
An update carries no content claim: in-flight subscriptions under the route are undisturbed.

~~~
ANNOUNCE_UPDATE Message {
  Type (i) = 0x2
  Message Length (i)
  Announce ID (i),
  Hop Count (i),
  Hop ID (i) ...,
  Warm Route Cost (i),
  Cold Route Cost (i),
}
~~~

**Type**:
Set to 0x2 to indicate an ANNOUNCE_UPDATE message.

**Announce ID**:
The ordinal implicitly assigned by a prior ANNOUNCE_START on this stream.
Referencing an id that was never assigned, or one already retired by an ANNOUNCE_END, is a protocol violation.

**Hop Count**, **Hop ID**, **Warm Route Cost**, and **Cold Route Cost**:
As defined for [ANNOUNCE_START](#announce-start).
An update whose only change is a Route Cost is valid: it is how a relay advertises that it started or stopped actively carrying content under the route.


## SUBSCRIBE
SUBSCRIBE is sent by a subscriber to start a subscription.

~~~
SUBSCRIBE Message {
  Message Length (i)
  Subscribe ID (i)
  Broadcast Path (s)
  Track Name (s)
  Subscriber Priority (8)
  Subscriber Max Age (i)
  Group Start (i)
  Group End (i)
  Frame Start (i)
  Frame End (i)
}
~~~

**Subscribe ID**:
A unique identifier chosen by the subscriber.
A Subscribe ID MUST NOT be reused within the same session, even if the prior subscription has been closed.

**Subscriber Priority**:
The priority of the subscription within the session, represented as a u8.
The publisher SHOULD transmit *higher* values first during congestion.
See the [Prioritization](#prioritization) section for more information.

**Subscriber Max Age**:
The subscriber's preference, in milliseconds, for how long a non-latest group may remain in flight before being considered stale and dropped from live delivery.
The publisher SHOULD reset (at the QUIC level) Group Streams for groups whose age relative to the latest group exceeds this duration.
Applies only to non-latest groups; the latest group is never dropped on staleness grounds.
A value of `0` means the subscriber wants only the latest group in live delivery (older groups are immediately stale once a newer group arrives).
This is a delivery-time preference, not a retention rule: the publisher MAY still hold these groups for FETCH or future subscriptions (see `Publisher Max Age` in [TRACK_INFO](#track-info)).
See the [Expiration](#expiration) section for more information.

**Group Start**:
The minimum group sequence to deliver: an absolute floor, defaulting to 0 (no floor).

A floor is not a request; `Subscriber Max Age` is the only thing that asks for data.
The publisher SHOULD start at the oldest group at or above the floor that [Expiration](#expiration) has not expired, and MUST NOT deliver an older one.
A `Subscriber Max Age` of 0 therefore starts at the latest group, since every older group is already stale.
A subscriber that buffers is then handed the head of what it can still play instead of only the live edge, and is never sent history it would discard on arrival: the same bound decides what to start at and what to expire, so the two cannot disagree.
A floor above the latest group simply waits there: that is a resumed subscription naming where it left off.
Reaching back is best-effort, not a guarantee that the groups still exist; see `Publisher Max Age` in [TRACK_INFO](#track-info).

**Group End**:
The last group to deliver (inclusive).
A value of 0 means unbounded (default).
A non-zero value is the absolute group sequence + 1.

**Frame Start**:
The index of the first frame to deliver within the `Group Start` group (see [Positions](#positions)).
A value of 0 means from the start of that group (default), so the group is delivered whole.
Frames before this index are not delivered, even when delivery begins at that exact group; a start resolved at a later group is always delivered from frame 0.
A subscriber that has received no group MUST send 0, since it cannot number the frames of a group it has not seen.

**Frame End**:
The last frame to deliver (inclusive) within the end group (see [Positions](#positions)).
A value of 0 means the whole group (default).
A non-zero value is the absolute frame index + 1, matching `Group End`.
MUST be 0 when `Group End` is 0, since an unbounded subscription has no end group to qualify.

`Group Start` and `Group End` are offset by 1 only so 0 can mean "absent"; every other group field in this document is a plain absolute sequence.

## SUBSCRIBE_UPDATE
A subscriber can modify a subscription with a SUBSCRIBE_UPDATE message.
A subscriber MAY send multiple SUBSCRIBE_UPDATE messages to update the subscription.
The start and end group can be changed in either direction (growing or shrinking).

~~~
SUBSCRIBE_UPDATE Message {
  Message Length (i)
  Subscriber Priority (8)
  Subscriber Max Age (i)
  Group Start (i)
  Group End (i)
  Frame Start (i)
  Frame End (i)
}
~~~

See [SUBSCRIBE](#subscribe) for information about each field.
Moving `Frame Start` forward within a group that is already being delivered does not retract frames the publisher has already sent; like `Group Start`, it only bounds what the publisher sends from here on.


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
  Publisher Max Age (i)
  Timescale (i)
}
~~~

Every field is **fixed for the lifetime of the Track** and MUST NOT change; a change requires a new Track.
This is what lets the properties live on their own stream, fetched once and cached, instead of being echoed on every SUBSCRIBE and FETCH response.
Publisher properties fan *out* at a relay (one upstream subscription serving many downstreams), so a change would have to propagate everywhere; subscriber properties fan *in*, which the relay already merges, so they MAY change freely via SUBSCRIBE_UPDATE.

**Publisher Priority**:
The publisher's priority for this Track, represented as a u8, used only to resolve ties between subscriptions of equal subscriber priority.
See the [Prioritization](#prioritization) section for more information.

**Publisher Max Age**:
The maximum age, in milliseconds, that the publisher caches a non-latest group past the arrival of a newer group.
Applies only to non-latest groups; the latest group is always retained.
It is an upper bound on retention, the inverse of an HTTP `Cache-Control: max-age` guarantee:

- A subscriber MAY issue a SUBSCRIBE or FETCH with an older `Group Start`, but the publisher MAY have already dropped any group whose age exceeds `Publisher Max Age`.
- The publisher MAY drop groups sooner than `Publisher Max Age` under resource pressure; subscribers MUST NOT assume older groups within the bound are still available.

A value of `0` means the publisher caches only the latest group (older groups MAY be dropped as soon as a newer group arrives).
The unit is milliseconds, matching `Subscriber Max Age`.
See the [Expiration](#expiration) section for more information.

**Timescale**:
The number of timestamp units per second for frame timestamps on this Track.
It MUST be non-zero; a subscriber that receives 0 MUST reset the stream with a protocol violation.
Common values include `1000` (milliseconds), `1000000` (microseconds), `48000` (audio sample rate), and `90000` (RTP video clock).

## SUBSCRIBE_OK {#subscribe-ok}
A SUBSCRIBE_OK message confirms a subscription and resolves its absolute start position.
It is the first message the publisher sends on the Subscribe Stream, once the start position is known.

This is the trimmed-down counterpart of MoqTransport's SUBSCRIBE_OK: it retains the name and the role of the publisher's positive response, but carries only the resolved start position (all other per-track properties live in [TRACK_INFO](#track-info)).

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

There is no matching frame field, because the start frame is never in doubt: a partial group is only delivered when it was asked for, so the subscription starts either exactly where it asked or at the beginning of a later group (see [Positions](#positions)).
The subscriber derives the start frame from `Group` and its own request:

- `Group` equals the requested start group: delivery begins at the requested `Frame Start`.
- `Group` is greater: delivery begins at frame 0.

The second case is easy to get wrong, so to be explicit: a subscriber that requested group 5 frame 15 and receives `Group` = 6 starts at **frame 0** of group 6, not frame 15.
The frame offset belonged to group 5 and is gone along with the rest of it; it does not carry forward to whichever group the publisher resolved to.

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
  Frame Start (i)
  Frame End (i)
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

**Frame Start**:
The index of the first frame to return within the group (see [Positions](#positions)).
A plain absolute index; 0 means the start of the group (default).

**Frame End**:
The last frame to return (inclusive), encoded as the absolute frame index + 1.
A value of 0 means through the end of the group (default).
A `Frame End` below `Frame Start` once decoded is a protocol violation; equal bounds are a legal single-frame range.

The publisher responds with FRAME messages directly on the same stream — there is no response header.
The subscriber parses them using the track's [TRACK_INFO](#track-info), which it MUST already have (see the [Track Stream](#track-stream)); the group sequence and the index of the first frame are implicit from the FETCH request.
The publisher FINs the stream after the last frame, or resets on error.
There is no FETCH_ERROR message — the publisher signals failure by resetting the stream.
A publisher holding fewer frames than requested MUST reset rather than truncate, since a short response is indistinguishable from one that started elsewhere.
A group that ends before `Frame End` is not a truncation: the publisher FINs after the last frame it has, provided the group is complete and it served everything from `Frame Start` onward.

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

## GOAWAY {#goaway-message}
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
The URI MUST NOT exceed 8,192 bytes; a receiver MUST treat a longer URI as a protocol violation and MAY reject it based on the length prefix alone.
A recipient MUST validate the URI against local policy before reconnecting, including verifying the scheme, authority, and port are permitted.
If validation fails, the recipient MUST close the session without reconnecting.

A client MUST send an empty New Session URI, as it cannot instruct a server to establish connections.
A server that receives a non-empty New Session URI MUST close the session with a protocol violation.

The new session URI SHOULD use the same scheme as the current session's URI.

An endpoint MUST close the session with a protocol violation if it receives more than one GOAWAY.

A peer that reconnects to a provided URI SHOULD keep using that URI for subsequent reconnects rather than reverting to the original.

A relay that receives a GOAWAY SHOULD treat the announcements that arrived on that session as the most expensive routes available, so a subscription it can serve from another session moves at the next Group boundary rather than when the draining session finally closes.
The routes stay usable: a broadcast reachable only over the draining session MUST keep being served until the session ends, which is what makes the sender's deadline a handover window rather than a cutoff.

## GROUP
The GROUP message contains information about a Group, as well as a reference to the subscription being served.

~~~
GROUP Message {
  Message Length (i)
  Subscribe ID (i)
  Group Sequence (i)
  Frame Start (i)
}
~~~

**Subscribe ID**:
The corresponding Subscribe ID.
This ID is used to distinguish between multiple subscriptions for the same track.

**Group Sequence**:
The sequence number of the group.
This SHOULD increase by 1 for each new group.
A subscriber MUST handle gaps, potentially caused by congestion.

**Frame Start**:
The index of the first FRAME message on this stream within the group (see [Positions](#positions)).
A plain absolute index; 0 means the stream carries the group from its beginning, which is the common case.
A non-zero value means the leading frames are not on this stream, either because the subscription started partway into this group or because the publisher only holds part of it.
The subscriber MUST NOT assume it will receive the missing frames on another stream; they are a gap unless a separate subscription or FETCH covers them.


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
- Made a repeated non-zero Hop ID in one announcement's Hop ID list a PROTOCOL_VIOLATION, matching draft-lcurley-moq-cluster. Repeated 0 entries stay legal.
- Moved the Qmux-over-WebSocket binding details to draft-lcurley-qmux-websocket; the binding itself is unchanged.
- Extended the SETUP `Path` parameter to carry the URI query: a client appends `?` and the query component after the path, matching moq-transport's PATH option. The credential a deployment puts in the query was previously unrepresentable on a binding with no request URI.
- Allowed an empty SETUP `Path` parameter, equivalent to omitting it; both request the server's default path. Previously an empty value was a protocol violation, which made the two ways of asking for the default disagree.
- Corrected SUBSCRIBE_END `Group` to an exclusive bound: the first sequence that will never be delivered, with 0 meaning no groups were produced. It was previously specified as the inclusive last group, which could not distinguish an empty track from one whose only group was 0.
- Split ANNOUNCE_BROADCAST into three typed messages: ANNOUNCE_START (0x0), ANNOUNCE_END (0x1), and ANNOUNCE_UPDATE (0x2), each prefixed with a Type discriminator like the subscribe stream's responses.
- Added implicit Announce IDs: each ANNOUNCE_START assigns the next per-stream ordinal.
- ANNOUNCE_END and ANNOUNCE_UPDATE reference the Announce ID instead of repeating the broadcast path.
- Replaced the duplicate-`active` restart idiom with ANNOUNCE_UPDATE; a second ANNOUNCE_START for an already-advertised prefix is now a protocol violation.
- Made the Announce Stream violations close the session rather than reset the stream: an unknown or retired Announce ID, an ANNOUNCE_START for an already-advertised prefix, and any announcement before ANNOUNCE_OK.
- Redefined an announcement as a route: the advertised path is a prefix claiming that paths beneath it can be served, matching moq-transport's namespace semantics, rather than the exact path of one available broadcast. A route claims capability, not inventory; which covered paths name broadcasts is an application convention (announcing each broadcast's exact path keeps enumeration working). ANNOUNCE_UPDATE updates a route's metadata in place and carries no content claim. Because routes carry no content identity, a relay never splices a live subscription across sources reached through different routes: a serving session ending resets its subscriptions and the subscriber re-requests through the best remaining route.
- Stated the ended-broadcast lifecycle without a flag: a broadcast whose route is retracted with ANNOUNCE_END may remain readable by exact path, its stored groups read over FETCH, discovered out of band; retraction never disturbs in-flight subscriptions.
- Specified route matching as per path segment (a prefix never matches half a segment) and specified how a route broader than the requested prefix presents: clamped to the request, as an empty suffix.
- Added `Warm Route Cost` and `Cold Route Cost` fields to ANNOUNCE_START and ANNOUNCE_UPDATE: the same path priced against two cache states. Warm is the accumulated cost of the transfers a subscription via this advertisement would newly cause, and is what route selection minimizes; Cold prices the path as if nothing along it were carrying, and breaks a Warm tie before path length, with the most recently received advertisement below that. Only Warm takes the actively-carrying discount, so Cold still ranks two relays that both advertise 0, and a relay adopts another carrying relay only when that relay's `(Cold cost, hash)` rank is strictly lower. A wire that cannot express Cold is read as the saturation ceiling, not as 0.
- Added a SETUP `Cost` parameter (0x4) declaring what subscribing from the sender costs, added by the receiver to every announcement that sender forwards. Both endpoints send their own, so the two directions are priced independently, and a receiver MAY charge a locally configured value instead. Unpriced directions default to 1, degrading to shortest-path routing.
- Removed `Exclude Hop` from ANNOUNCE_REQUEST. The receiver's hop-based loop check already discards a looped announcement, so the field only saved the wasted send.
- Stated the receiver's loop check normatively in ANNOUNCE_START: an announcement whose reconstructed path contains the receiver's own Hop ID is neither forwarded nor selected as a route.
- Added a SETUP `Hop` parameter (0x5): each endpoint declares its Hop ID at session setup, carrying session-wide the identity `Exclude Hop` carried per announce stream, and filtering subscriptions as well as announcements (including sessions that never open an Announce Stream).
- Made advertisement selection per subscriber: a publisher MUST NOT advertise a path containing the subscriber's declared Hop ID and otherwise advertises the best remaining one (a subscriber the serving path flows through receives the best standby instead of nothing), MUST serve subscriptions by the same exclusion, and the actively-carrying cost discount applies only to the serving path. This is how redundant publishers fail over across a mesh.
- Added `Frame Start` and `Frame End` to SUBSCRIBE and SUBSCRIBE_UPDATE, qualifying the start and end group with a frame index so a subscription can begin or end partway through a group. `Frame Start` is a plain index qualifying the `Group Start` group; `Frame End` is the index + 1, matching `Group End`, and MUST be 0 when `Group End` is absent.
- Added `Frame Start` and `Frame End` to FETCH, bounding the returned frames within the group. A publisher that cannot serve the full range resets the stream.
- Added `Frame Start` to GROUP, giving the index of the first FRAME on the stream so a partial group is self-describing.
- Added the Positions section defining a (group, frame) position, its lexicographic ordering, and the rule that a partial group is only ever delivered to a subscriber that asked for one. A publisher that cannot serve a group from the requested frame skips it and resolves to a later group, so SUBSCRIBE_OK needs no frame field: the start frame follows from `Group` and the subscriber's own request.
- Capped the GOAWAY New Session URI at 8,192 bytes, matching moq-transport.
- Restricted the GOAWAY New Session URI to servers, specified a duplicate GOAWAY as a protocol violation, and recommended scheme continuity and sticky redirects.
- Exempted a ceiling-cost serving path from the actively-carrying cost discount: a relay whose serving path costs the saturation ceiling (primarily a session that received a GOAWAY) advertises the ceiling instead of 0, so the drain propagates downstream instead of being re-masked by each carrying hop. Keyed on the value, not the reason, which does not travel on the wire.
- Added the Error Codes section, defining separate session and stream code spaces and listing the codes moq-lite uses, reused unchanged from moq-transport. Codes 64+ are the application's; 32-63 are reserved and MUST NOT be interpreted, pending a future revision. Previously the codes were unspecified, so an endpoint could neither send one a peer would understand nor safely interpret one it received. Note this renumbers every code an existing implementation sent, and that a stream reset of 0x0 is now INTERNAL_ERROR rather than a cancellation (CANCELLED is 0x1).
- Renamed `Subscriber Max Latency` to `Subscriber Max Age` and `Publisher Max Latency` to `Publisher Max Age`.
- Redefined timestamp age so a group is expired only once it provably cannot overlap the Max Age window. Age is now measured from a group's *reach* (the first frame timestamp of its successor, which is the furthest it could still present) to the newest frame of the latest group, and an age equal to Max Age expires it, since reach is an exclusive bound. Previously age ran from the group's own first frame, which expired a group whose later frames were still well inside the budget. A group whose successor has not yet presented a frame is never expired, because frame durations are not on the wire and nothing else proves where it ends.
- Removed the wall-clock age measure from expiration: timestamps are the only input, and wall-clock reclamation of idle content is the retention cache's own policy (`Publisher Max Age`) rather than part of the subscription rule. A zero-frame group needs no measure of its own, since its reach is bounded by its successor's first frame like any other group.
- Removed `Subscriber Ordered` from SUBSCRIBE and SUBSCRIBE_UPDATE, and `Publisher Ordered` from TRACK_INFO and SUBSCRIBE_OK. Group order within a Track is now normatively newest-first, with no field to invert it: a subscriber that wants sequence order reads in sequence order, which costs the network nothing and does not let one subscriber's preference reach a Track a relay is fanning out to many. Note this removes a byte from the middle of each message, so a lite-06 peer cannot parse a lite-05 one's SUBSCRIBE (the earlier drafts keep the byte, and an implementation serving them SHOULD write 0 and ignore what it reads).
- Made `Group Start` an absolute floor (the raw minimum group sequence, default 0) rather than the sequence + 1 with 0 meaning the latest group. The start resolves from `Subscriber Max Age` instead: the oldest group at or above the floor within the budget, so a subscriber that buffers is handed the head of what it can still play. A zero budget still resolves to the latest group, which was the only start the old encoding could ask for by default.

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
Hop IDs (see [ANNOUNCE_OK](#announce-ok) and [ANNOUNCE_START](#announce-start)) expose the relay path of a broadcast, which may reveal internal topology. A relay that does not wish to disclose its position MAY use the reserved value 0 ("unknown") instead of a stable identifier, at the cost of losing loop detection through itself (see [Routing](#routing)). The Hop ID announcement filter (see [Hop Parameter](#hop-parameter)) exists for loop avoidance, not access control: a subscriber cannot verify that a publisher honored it, so it MUST NOT be relied upon to hide a broadcast from a peer that declared its Hop ID.

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
