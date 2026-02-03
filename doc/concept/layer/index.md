---
title: Layering
description: It's like a cake but reusable.
---

# Layers
It's like a cake but reusable.

The design philosophy of MoQ is to make things simple, composable, and customizable.
We don't want you to hit a brick wall if you deviate from the standard path (*ahem* WebRTC).
We also want to benefit from economies of scale (like HTTP), utilizing generic libraries and tools whenever possible.

To accomplish this, MoQ is broken into layers:

```text
┌─────────────────┐
│   Application   │   🏢 Your business logic
│                 │    - authentication, non-media tracks, etc.
├─────────────────┤
│  Media Format   │   🎬 Media-specific encoding/streaming
│     (hang)      │     - codecs, containers, catalog
├─────────────────├
│  MoQ Transport  │  🚌 Generic pub/sub transport
│   (moq-lite)    │     - broadcasts, tracks, groups, frames
├─────────────────┤
│  WebTransport   │  🌐 Browser-compatible QUIC
│                 │     - HTTP/3 handshake
├─────────────────┤
|      QUIC       |  🌐 Underlying transport protocol
│                 │     - streams, datagrams, prioritization, etc.
└─────────────────┘
```

You need to have some understanding of the responsibility and purpose of each layer to best utilize MoQ.
Let's dive in, starting at the bottom of the stack.


## QUIC
QUIC is the core protocol that powers HTTP/3, designed to fix head-of-line blocking that plagues TCP and thus HTTP/2.
It accomplishes this via multiple "streams" that can be delivered concurrently, with each HTTP/3 request using a dedicated stream.

QUIC uses UDP under the hood but it is fundamentally a connection-oriented protocol with congestion control, flow control, retransmissions, etc.
You should think of it like TCP 2.0 and not a firehose of UDP packets.

QUIC provides various forms of reliability:
- **Full Reliability**: A QUIC stream will be retransmitted until every byte arrives.
- **Partial Reliability**: A QUIC stream can be immediately RESET with an error code, aborting any forward progress.
- **No Reliability**: A QUIC datagram (extension) can be sent and will not be queued/retransmitted.

QUIC crucially decouples the delivery of streams.
QUIC streams do not block each other, unlike some other protocols that appear to offer concurrency (ex. SCTP, HTTP/2).
The sender can prioritize streams by deciding which packet to send next.
Either side can reset a stream to abruptly terminate it.

Okay those are cool features I guess, but why not use `<other_udp_protocol_here>`?

The crucial reason is that QUIC is a web standard:
- It has browser support in the form of HTTP/3 and WebTransport.
- It's been battle tested and optimized by huge CDNs.
- Every large tech company has open sourced their implementation: quiche, quiche (again), msquic, amazon, etc.

And frankly it's just a well designed protocol.
There's so many cool things you can do, like connection migration and anycast load balancing, that I don't have time to cover.
The big brains at the IETF took a wishlist of everything that *should* be in TCP and added it to QUIC.


## WebTransport
WebTransport is a small layer that shares a QUIC connection with HTTP/3.
It adds some features, mostly in the form of the HTTP handshake, but it primarily provides browser support for the QUIC API.
Think of it like WebSocket but for QUIC instead of TCP.

In my opinion, it's not particularly well designed but also harmless.
Most of the complexity can be abstracted away if you use my [web-transport](/rs/crate/web-transport) libraries.
Native MoQ clients can also skip WebTransport entirely and use QUIC directly via an ALPN.

But why doesn't MoQ use HTTP/3 instead of WebTransport?
It's certainly possible but HTTP semantics make it more difficult:

- **With WebTransport**: both sides can create streams whenever and immediately write new frames.
- **With HTTP/3**: only the client can create a stream (HTTP request), as HTTP push is gone and a mistake anyway. It gets awkward because the client needs to know when the server wants to write a new stream.

[moq-relay](/app/relay/) does provide a HTTP endpoint so a client can still request content on-demand instead of subscribing.
This is useful for backwards compatibility with HLS, but the long-term goal is to make publishing and subscribing symmetrical via WebTransport.


## MoqTransport
After a decade in the industry, I've come to realize something important:

> "economies of scale are gud" -- kixelated

The industry switched from RTMP to HLS mostly because of Apple, but it had a hidden benefit:
- RTMP was expensive to scale because it supported media use-cases only.
- HTTP was cheap to scale because it supported the entire internet.

A protocol like WebRTC that combines networking and media can scale in theory, but it doesn't in practice:
- The media specifics make it more complicated.
- The limited use-cases (conferencing) makes it less profitable.

Our goal with the MoQ Transport layer is to only include the bare minimum required for a relay/CDN to operate.
It's a protocol devoid of codecs or containers, abstracting them away into raw byte streams.
The relay still needs some information, like GoP/frame boundaries and priorities, but these are represented as generic headers.

[MoqTransport](/concept/standard/moq-transport) is a generic pub/sub protocol that can support a wide array of use-cases.
However, I think it's too complicated and there's too much churn, so I'm using a subset called [moq-lite](/concept/layer/moq-lite).
Click those links for a breakdown of how the live content is further sub-divided and delivered.

The other day somebody said they were looking into using `moq-lite` for database replication.
That means the design is working.


## Media Format
We've come to the `M` in `MoQ`.
Which ironically is the least standardized layer at the moment, as the primary focus is on the transport.

A relay might not care about the media details, but the end clients need to agree on something.
The publisher has to encode the media in such a way that a viewer can decode (and render) the media.

In general, the approach is to break the media format into more layers:
- **Playlist**: A description of media tracks, each using a specified container/codec. For example: HLS
- **Container**: A description of media frames and metadata. For example: fMP4
- **Codecs**: The low-level frame compression. For example: H.264

For MoQ, we wanted to make a few improvements:
- **Playlist**: Allow live updates so tracks can be added/removed/changed on the fly.
- **Container**: Support frame-by-frame streaming to minimize latency.
- **Codec**: Split at group and frame boundaries, so MoqTransport can deliver efficiently.

I created [hang](/concept/layer/hang) for this purpose.
It's very simple: a JSON playlist to describe the tracks and a container for each frame.
The IETF is working on a [suite of drafts](/concept/standard/msf) but the ideas are similar.

And note, you can make your own media format if you have full control over the publisher and all viewers.
You would be missing out on existing tools and libraries but it's really not that complicated;
QUIC and MoqTransport do the heavy lifting.


## Application
Finally, we're here at your application.

One of the explicit goals for MoQ is to avoid the need to petition Google and Apple to add new features.
If you want to do something custom with MoQ, or go slightly off the beaten path, then you absolutely can.

The principle is that MoQ tracks are additive.
You can create new tracks for whatever purpose and it doesn't interfere with the function of a relay or media client.
Go ahead, create a `controller` track to stream button presses and it will be treated like any other opaque sequence of bytes.

Additionally, MoQ is implemented in application space.
It's not an unchangeable side-car like WebRTC, built into the browser directly.
If you don't like some functionality in this library, go ahead and fork it.
You can ship your own web and native apps.

The one exception is that you can't modify QUIC/WebTransport within web browsers; still have to petition Google and Apple for that.
Native implementations can ship custom QUIC/WebTransport stacks though if you really want to mess with custom congestion control or something.

## More Info
If you're interested, you can dive deeper into specific layers and read the actual specs.
I'm biased and would recommend starting with the minimal:
- [moq-lite](/concept/layer/moq-lite)
- [hang](/concept/layer/hang)

Then you should read more about the various [IETF standards](/concept/standard/).
