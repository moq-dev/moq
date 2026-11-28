---
title: "Client/Server Sockets over MoQ"
abbrev: "moq-socket"
category: info

docname: draft-lcurley-moq-socket-latest
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
  moql: I-D.lcurley-moq-lite

informative:
  RFC768:
  RFC9000:
  RFC9293:
  moqhidden:
    title: "MoQ Hidden Extension"
    target: https://datatracker.ietf.org/doc/draft-lcurley-moq-hidden/
    author:
      -
        ins: L. Curley
        name: Luke Curley
    date: false

--- abstract

This document describes how to build point-to-point client/server sockets over MoQ Transport {{moqt}} and moq-lite {{moql}} without any wire extensions.
Endpoints advertise hidden role namespaces (`.server` and `.client`) and establish bidirectional sessions as pairs of unlisted tracks through a mutual subscription handshake.
Because every session uses ordinary tracks, it traverses unmodified relays and CDNs, reuses namespace-prefix authorization, and supports arbitrary framing per direction.
This document specifies the rendezvous and replay-protection convention, then illustrates it with reliable byte streams (TCP), unreliable datagrams (UDP), and the flow-control and relay-caching considerations they raise.

--- note_Note_to_Readers

This document is maintained alongside the reference implementation at [github.com/moq-dev/moq](https://github.com/moq-dev/moq).
Submit an [issue](https://github.com/moq-dev/moq/issues) or [PR](https://github.com/moq-dev/moq/pulls) if this spec sucks and you want to fix anything.

--- middle

# Conventions and Definitions
{::boilerplate bcp14-tagged}

This document applies to both MoQ Transport {{moqt}} and moq-lite {{moql}}, using the following common terms:

Namespace:
: A Track Namespace in {{moqt}} or a Broadcast Path in {{moql}}.

Advertise:
: Announcing a namespace so relays can route subscriptions to it (`PUBLISH_NAMESPACE` or `NAMESPACE` in {{moqt}}; `ANNOUNCE` in {{moql}}).

Endpoint:
: A participant that accepts connections (a **server**), initiates connections (a **client**), or both, identified by an **Endpoint ID**.

Session:
: One point-to-point connection between a **dialer** and an **acceptor**, carried as two independent tracks, one published by each side.

Unlisted track:
: A track served on demand when requested by name, without advertising it in track listings or catalogs.


# Introduction

MoQ is built for 1-to-N, pull-based delivery: a publisher advertises a namespace and subscribers pull tracks through relays that cache and fan out groups.
Many applications on the same network also need 1-to-1 client/server flows, such as RPCs, remote control, device access, or TCP and UDP tunnels.

Adding a dedicated bidirectional primitive to MoQ requires every hop to implement new wire messages, routing, flow-control, and authorization rules, and locks traffic into one delivery model (either ordered bytes or a bespoke frame format).
Instead, client/server flows can run directly over existing MoQ tracks:

- No protocol extensions are required; sessions traverse unmodified relays and CDNs.
- Ordinary namespace-prefix grants control which endpoints may listen and which may dial.
- Each half of a session is a full MoQ track, so an application chooses ordered streams, prioritized groups, expiration budgets, or datagrams per protocol.

The trade-off is one extra round trip during session setup (for mutual verification) and a namespace advertisement per active client.
The rest of this document specifies the namespace layout, handshake, replay defenses, TCP and UDP mappings, and the flow-control behavior required for bulk streams through caching relays.


# Namespaces and Roles {#namespaces}

## Endpoint IDs

Every endpoint has an Endpoint ID consisting of a single non-empty path segment containing neither `/` nor `\`.
A server typically uses a well-known name; a client uses an identity-derived or randomly generated ID.

## Layouts

Each endpoint uses up to two namespaces, one per role, placed under the reserved segments `.server` and `.client`:

Role layout:
: `<prefix>/.server/<id>` and `<prefix>/.client/<id>`, where `<prefix>` is a service-defined namespace prefix (possibly empty).
  Putting the role before `<id>` lets simple prefix grants separately authorize listeners (`<prefix>/.server/<id>`) and dialers (`<prefix>/.client/<id>`).

Owned layout:
: `<prefix>/<id>/<service>/.server` and `<prefix>/<id>/<service>/.client`, used when each endpoint already holds a grant covering everything beneath `<prefix>/<id>`.

All endpoints participating in a service MUST agree on the layout.
The examples below use the role layout; the owned layout is identical except for the namespace paths.

## Hidden Namespaces and Lazy Advertisement

The leading `.` in `.server` and `.client` marks these namespaces as hidden from default discovery ({{moqhidden}}, and hidden broadcasts in {{moql}}).
Applications that list broadcasts under `<prefix>` will not see socket endpoints, while relays still route explicit subscriptions to them normally.
Hiding is a discovery convenience, not access control ([Security Considerations](#security)).

An endpoint SHOULD advertise each role namespace lazily:

- Advertise `<prefix>/.client/<id>` on its first dial, not at startup.
- Advertise `<prefix>/.server/<id>` when it starts accepting dials.

A pure client therefore never advertises a server namespace, and a pure server never advertises a client namespace.


# Session Handshake {#handshake}

## Track Naming

Both halves of a session use the exact same unlisted track name, once in `<prefix>/.client/<dialer>` and once in `<prefix>/.server/<acceptor>`:

~~~
<protocol>/<dialer>/<acceptor>/<session>
~~~

`<protocol>`:
: A single path segment naming the framing carried on both tracks (for example `stream` or `datagram`).

`<dialer>` and `<acceptor>`:
: The Endpoint IDs of the initiating client and the listening server.

`<session>`:
: `<timestamp>-<random>`, where `<timestamp>` is the dialer's wall-clock time in whole seconds since the Unix epoch (lowercase hexadecimal) and `<random>` is 128 bits of cryptographically random entropy (32 lowercase hexadecimal characters).

Every segment MUST be non-empty and contain neither `/` nor `\`.
Session tracks MUST NOT be advertised in catalogs or track listings.

## Establishing a Session

~~~
Dialer A (.client/A)                            Acceptor B (.server/B)
  |                                               |
  |  (reserve unlisted track T on .client/A)      |
  |--- SUBSCRIBE .server/B, track T ------------->|
  |                                               | (validate T, fresh,
  |                                               |  not replayed)
  |<-- SUBSCRIBE .client/A, track T --------------|
  |  (confirm A dialed T to B; serve half)        |
  |--- SUBSCRIBE_OK / SUBSCRIBE_START ----------->|
  |                                               | (serve half)
  |<-- SUBSCRIBE_OK / SUBSCRIBE_START ------------|
  |                                               |
  |<========== independent MoQ tracks ==========>|
~~~

1. **Dial:** Dialer `A` waits until `<prefix>/.server/<B>` is advertised, mints `<session>`, reserves `T = <protocol>/<A>/<B>/<session>` on `<prefix>/.client/<A>`, and sends `SUBSCRIBE` for `T` to `<prefix>/.server/<B>`.
   Minting `<session>` only once `B` is advertised ensures `<timestamp>` reflects when `B` can first hear the dial.
2. **Validate:** Acceptor `B` receives the incoming `SUBSCRIBE` on `<prefix>/.server/<B>` and checks:
   - The track name parses into four valid segments with `<acceptor> == B` and `<dialer> != B`. Otherwise `B` rejects with `NOT_FOUND`.
   - `<protocol>` is recognized and matches what the listener expects. Otherwise `B` rejects with `UNSUPPORTED` or a protocol-mismatch application error code.
   - `<session>` is fresh and has not been accepted before ([Replay Protection](#replay)). Otherwise `B` rejects with a non-retryable application error code.
3. **Callback:** To accept, `B` sends `SUBSCRIBE` to `<prefix>/.client/<A>` for the same track name `T`.
   Because `A`'s advertisement of `.client/<A>` can race with its dial across relays, `B` SHOULD wait for `.client/<A>` to become advertised for as long as `A`'s `SUBSCRIBE` remains open.
4. **Confirm:** `A` serves `T` on `.client/<A>` only if it has an active dial matching `T`, and rejects any other request with `NOT_FOUND`.
   Once `B`'s subscription to `.client/<A>` succeeds, `B` knows the holder of publish authority for `.client/<A>` actually dialed `B` with `<session>`, and `B` begins serving its half `T` on `.server/<B>`.
5. **Open:** Once both subscriptions are answered, each endpoint writes to its own track producer and reads from the peer's track subscription.

## Cancellation and Retries

If the dialer cancels its `SUBSCRIBE` before acceptance, the acceptor abandons the pending dial and cancels its callback `SUBSCRIBE`.
If the route to `.server/<B>` fails as unroutable before `B` requests `A`'s half, `A` MAY retry once `.server/<B>` is re-advertised, using a freshly minted `<session>`.

Once a session is rejected or either half ends, endpoints MUST signal resets with an application error code that relays do not retry.
Retrying an ended half causes a relay to re-send `SUBSCRIBE` to `.server/<B>`, which is indistinguishable from a new dial without replay protection.


# Replay Protection {#replay}

Relays may retry a subscription when an intermediate hop drops.
If an acceptor has already closed its half while the dialer's half is still open, a relay retrying `SUBSCRIBE` to `.server/<B>` would cause `B` to accept the same session a second time and replay the dialer's buffered bytes into a new connection.

An acceptor MUST accept each session at most once, using bounded memory:

1. **Freshness window:** Choose a clock-skew budget `DIAL_AGE` (for example 60 to 120 seconds, covering clock skew between endpoints plus queueing delay).
   Reject any dial where `|now - timestamp| > DIAL_AGE` using a non-retryable application error code.
2. **Accepted set:** Record each accepted track name along with its `<timestamp>`.
   Reject any duplicate request for a recorded track name with a non-retryable application error code.
3. **Bounded eviction and monotonic floor:** Evict entries whose `<timestamp>` is older than `now - DIAL_AGE`.
   Maintain a monotonic `floor` equal to the largest evicted `<timestamp>`, and reject any dial with `<timestamp> <= floor`.
   The floor ensures that stepping the acceptor's wall clock backward cannot make a forgotten session appear fresh again.

State is therefore bounded by the number of sessions accepted within `DIAL_AGE`, never by the lifetime number of sessions.

Naming both `<dialer>` and `<acceptor>` in the track name independently prevents cross-acceptor replay: if `B` attempts to replay `A`'s dial to a third endpoint `C`, `C` rejects `<acceptor> != C`, and `A` never reserved `<protocol>/<A>/<C>/<session>`.


# TCP over MoQ (`stream`) {#tcp}

A reliable, ordered byte stream with TCP semantics {{RFC9293}} maps onto a session with `<protocol> = "stream"`.

## Track Configuration and Framing

- **Subscription:** Both sides subscribe reliably, in ascending group order, from the start of the track, with no expiration delay budget so late frames are waited for rather than skipped.
- **Groups:** Each half carries one long-lived Group for the life of the connection (or rolling Groups when traversing relays with per-group size limits; see [Flow Control Considerations](#flow-control)).
  The initial Group ID SHOULD be chosen randomly (for example a uniform 32-bit integer) so a hop retry or restarted endpoint can never splice bytes from a prior session into an active reader.
- **Frames:** Each write produces one Frame.
  Writes larger than a bounded maximum frame size `MAX_FRAME` (64 KiB is a practical default matching typical socket copy buffers) MUST be split into multiple frames to limit per-frame serialization latency and relay cache slot granularity.
  Readers SHOULD return bytes as chunks of a frame arrive rather than buffering an entire frame.

## Half-Close (FIN) and Reset (RST)

Clean half-close (FIN):
: To shut down its write half, an endpoint finishes its Group with an explicit end marker (for example `END_OF_GROUP` in {{moqt}}, or a declared end of `start + 1` in timestamp-addressed {{moql}}) and finishes the Track.
  The peer reads EOF once all bytes before that end marker are consumed, while the reverse half remains open.
  If a Group ends without the writer's explicit end marker, such as when a relay drops a partial copy, the reader MUST treat it as a connection reset rather than clean EOF.

Abrupt reset (RST):
: To reset the stream, an endpoint aborts its write Track with a non-retryable application error code (for example `0` for an unspecified reset).
  Any error on either half resets the entire byte stream: the endpoint aborts its own write half and reports a connection reset to both readers and writers.
  Dropping a byte stream before cleanly shutting down its write half MUST emit a reset rather than finishing the track.


# Flow Control Considerations {#flow-control}

Byte streams highlight a core mismatch between live media relays and TCP: media relays cache groups to decouple fast publishers from slow or late viewers, whereas TCP needs lossless end-to-end backpressure with bounded per-hop memory.

## Hop-by-Hop QUIC Backpressure

QUIC {{RFC9000}} stream flow control operates per hop, not end to end.
If a relay reads an upstream Group stream at full speed into a large memory buffer regardless of downstream progress, a slow receiver causes the relay to fill its cache and evict frames (`EVICTED`), breaking the byte stream.

End-to-end backpressure emerges without wire changes when every relay along the path **paces** upstream reads against its fastest active reader:

- Each hop stops reading from its upstream Group stream once buffered bytes ahead of the fastest reader reach a pacing window (bounded by the transport's stream receive window).
- For a 1-to-1 socket track there is only one reader, so "fastest reader" is that reader.
- Backpressure therefore propagates hop by hop back to the sender, which pauses writes until the peer drains its stream.

## Relay Cache Footprint

Media relays commonly retain up to a full media group in memory (for example 32 MiB to cover a high-bitrate 4K group of pictures) so a subscriber joining mid-group receives the keyframe.
A 1-to-1 byte stream never has mid-group joiners; keeping 32 MiB of already-delivered frames behind the sole reader multiplies memory usage across every hop.

To keep 1-to-1 flows lightweight without hurting media mid-group joins:

- Relays SHOULD shed frames already consumed by all active readers when mid-group caching is not required, keeping only a flow-control window (for example 1 to 4 MiB) rather than the full media group cap.
- Where a protocol supports per-track retention bounds (such as `MAX_CACHE_DURATION` in {{moqt}} or a per-track tail bound), byte-stream tracks SHOULD advertise the smallest retention compatible with their flow-control window.
- Local writers SHOULD use a modest pacing window ahead of their reader (for example 1 to 2 MiB) rather than half of a 32 MiB media cache.

## Traversing Uncooperative Relays

A generic relay may not pace upstream reads against readers, or may enforce a hard per-group size cap (aborting a multi-gigabyte single group with `GROUP_TOO_LARGE`).
Two application-level techniques preserve reliability across such relays:

Rolling groups:
: Instead of one group for the entire connection, close the current Group and open the next consecutive Group ID after every N bytes (for example 1 MiB).
  Each group stays below typical per-group caps, and relays that only evict whole groups can reclaim finished segments.
  Readers reassemble Groups strictly in ascending Group ID order.
  Clean FIN is signaled by finishing the Track after the final Group.

Application-layer credit:
: If a relay buffers upstream frames faster than the reader drains them and evicts unread data, the peer can send periodic byte-credit updates (similar to HTTP/2 or SSH `WINDOW_UPDATE` frames) on the reverse track.
  The writer pauses once bytes written exceed the peer's credited offset, guaranteeing lossless end-to-end flow control even through relays that perform no backpressure of their own.

## Dead-Reader Detection

If the peer stops reading (drops its subscription) without closing its write half, flow control stops advancing.
A writer MUST monitor subscriber demand on its write track and treat the loss of all subscribers as a connection reset rather than blocking indefinitely waiting for credit.


# UDP over MoQ (`datagram`) {#udp}

Unreliable datagram delivery {{RFC768}} maps onto a session with `<protocol> = "datagram"`.
Unlike byte streams, UDP over MoQ involves trade-offs between payload size, head-of-line blocking, and connectionless state.

## Wire Mappings

Two mappings are practical, depending on payload size and transport support:

One-frame Groups (QUIC streams):
: Each datagram is written as its own Group containing a single Frame, starting from a random Group ID incremented by one per datagram.
  This supports the full IPv4 UDP payload limit of 65,507 bytes and works across all MoQ transports (including stream-only bindings such as WebSocket).
  Each datagram rides its own QUIC stream, avoiding cross-datagram head-of-line blocking while letting hop-by-hop QUIC retransmission recover brief packet loss.
  To prevent stale datagrams from queuing during congestion, both publisher retention (`Max Age`) and subscriber delivery budget are set to a short late threshold `LATE` (for example 100 ms): once a newer datagram overtakes an older one by more than `LATE`, relays and readers drop the late datagram.
  Readers MUST reject frames larger than the maximum datagram size or groups containing more than one frame as protocol violations.

MoQ Datagrams:
: Each datagram is sent on a native MoQ Datagram track ({{moqt}} / {{moql}}).
  This avoids per-datagram stream overhead and provides true single-transmission unreliable delivery, but payloads are bounded by the path QUIC or WebTransport datagram MTU (typically around 1200 bytes) and fail over transports without datagram support.

## Caveats

UDP over MoQ introduces three edge cases that applications must handle explicitly:

Latest-group retention:
: MoQ caches normally protect a live track's newest Group from age-based eviction.
  When a UDP flow goes idle, the last datagram remains cached indefinitely and may be delivered stale after a route switch or reconnect.
  With one-frame Groups, a sender can write an empty Group (zero frames, ignored by readers) after an idle interval so the preceding datagram is superseded and ages out after `LATE`.

Connectionless lifecycle:
: UDP has no connect or close handshake.
  A UDP-to-MoQ proxy must map source/destination tuples to synthetic sessions, accept dials lazily, and close idle track pairs after an inactivity timeout.
  If a session resets unexpectedly, the proxy drops its mapping and re-dials on the next outgoing datagram.

Congestion control:
: Native MoQ Datagrams carry no built-in end-to-end loss feedback, so an unpaced sender can overwhelm a constrained hop without knowing packets are being dropped.
  One-frame Groups inherit hop-by-hop QUIC congestion control and shed excess rate via `LATE`.


# Security Considerations {#security}

## Authorization

Hidden namespaces (`.server`, `.client`) provide no confidentiality or access control.
Relays MUST apply the same publish and subscribe authorization to hidden namespaces as to any other.
Separating `.server/` from `.client/` allows prefix grants to restrict which identities may serve a given Endpoint ID and which may dial it.

## Spoofing and Replay

The callback `SUBSCRIBE` to `<prefix>/.client/<dialer>` verifies that an entity authorized to publish `<prefix>/.client/<dialer>` actually initiated the dial.
An attacker who cannot publish under `<prefix>/.client/<A>` cannot complete a handshake claiming Endpoint ID `A`.

Embedding both `<dialer>` and `<acceptor>` in the track name prevents a malicious acceptor `B` from replaying `A`'s dial to a third endpoint `C`.
The timestamped `<session>`, `DIAL_AGE` window, accepted-session cache, and monotonic floor ([Replay Protection](#replay)) prevent an acceptor from accepting the same session twice across relay retries.

## Confidentiality and Untrusted Relays

The 128-bit random component of `<session>` prevents off-path peers from guessing active session track names.
Implementations SHOULD NOT log full session track names.

Relays forward plaintext MoQ payloads and, if they control namespace grants, can observe or impersonate endpoints.
When relays are not trusted for confidentiality or endpoint identity, applications MUST authenticate peers and encrypt payloads end to end inside the session (for example using TLS or Noise over a `stream` session).

## Resource Exhaustion

An attacker authorized to subscribe to `<prefix>/.server/<B>` can open many dials without completing them.
Acceptors SHOULD bound the queue of pending unaccepted dials, remove dials whose subscriber cancels, and rate-limit handshakes per dialer.


# IANA Considerations

This document makes no IANA requests.


--- back

# Acknowledgments
{:numbered="false"}

This document was drafted with the assistance of Gemini, an AI assistant by Google.
