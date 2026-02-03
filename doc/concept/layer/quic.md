---
title: QUIC
description: The transport protocol that makes MoQ possible
---

# QUIC

[RFC9000](https://datatracker.ietf.org/doc/html/rfc9000) - QUIC: A UDP-Based Multiplexed and Secure Transport

QUIC is why MoQ exists.
It's the protocol that finally grants us the web support needed for real-time streaming.

## Web History
To explain the purpose of QUIC, it's helpful to understand the history of HTTP.
Let's take a quick trip through history:

**HTTP/1** - One request at a time per TCP connection. Want to load 10 images? You either suffer from head-of-line blocking or open 10 connections, each with an expensive TCP/TLS handshake.
**HTTP/2** - Multiplexing! Multiple requests over one TCP connection. But wait... TCP still delivers bytes in order, so one lost packet blocks *everything*. We traded one form of head-of-line blocking for another.
**HTTP/3** - Built on QUIC with multiplexed streams that are truly independent. A lost packet in one stream doesn't block the others.

The media landscape is more varied but shares a lot in common.
RTMP and HLS appear demuxed like HTTP/2, but suffers the same fate.
Old audio/video frames end up blocking new frames from being delivered, driving up latency during congestion.

## Why Not Raw UDP?
"Just use UDP" has been the rallying cry of real-time media for decades.
That's exactly the route that protocols like WebRTC and SRT have taken.

However, the scope of the media protocol grows dramatically:
- You need congestion control
- You need encryption
- You need retransmissions
- You need prioritization
- You need NAT traversal
- You need browser support
- etc.

Only Google (WebRTC) has managed to navigate these problems and create a solid protocol that works in browsers.
The point of MoQ is to avoid reinventing the wheel and focus on **media** instead of **networking**.
QUIC is a fantastic protocol with wide support and the features we need.

## Features
A QUICk summary of QUIC features and how they are used in MoQ.

## Streams
After establishing a QUIC connection, both sides can create streams.
This is can be done instantly (without overhead) provided the configurable limit has not been reached.

There's two flavors of streams:
- **Bidirectional**: A stream that can be read from and written to.
- **Unidirectional**: A stream that can only be written to.

In MoQ, we use bidirectional streams for control messages and unidirectional streams for subscription data.

Each stream is a reliable sequence of bytes, with any gaps automatically retransmitted.
A stream can be closed to mark the final size, or it can be reset (by either side) to immediately terminate it.

Each stream is *mostly* independent of each other, containing its own data and flow control.
A lost packet on stream A doesn't stall stream B, nor do they have to be retransmitted together.
Stream A can be closed or reset independently of stream B.

In MoQ, we create a new stream for each video Group of Pictures.
All frames within a GoP are reliably delivered in order so the decoder will not error.
But Group A won't block Group B, nor will Track A block Track B.

### Reliability
QUIC provides three flavors of reliability:

- **Full Reliability**: A QUIC stream will be retransmitted until every byte arrives. Perfect for mandatory data like the catalog.
- **Partial Reliability**: A QUIC stream can be immediately RESET with an error code, aborting any forward progress. Perfect for skipping old video frames when you're behind - just reset the stream and move on.
- **No Reliability**: QUIC datagrams (an extension) can be sent without any queuing or retransmission. Fire and forget when retransmissions are not desired.

MoQ primarily uses partial reliability, reseting streams once the content is no longer desired (max latency).
However, in order to support multiple different latency targets, we prefer to prioritize streams instead of resetting them.

### Prioritization
The QUIC library is responsible for constructing and sending each UDP packet.
This means a QUIC library can prioritize streams by deciding which packet to send next.

This is useful in order to:
- Prioritize audio over video
- Prioritize recent frames over old frames

MoQ uses this extensively.
When the network is congested, old groups get starved while new groups get through immediately.
We'll eventually reset old groups once a maximum latency is reached, but it's better to prioritize than to reset.


## Browser Support

QUIC is available in browsers via [WebTransport](https://developer.mozilla.org/en-US/docs/Web/API/WebTransport_API):

- **Chrome/Edge** - Supported since 2021
- **Firefox** - Supported since 2023
- **Safari** - [Intent to support](https://github.com/WebKit/standards-positions/issues/18#issuecomment-1495890122), behind a flag since 2025

The lack of Safari support is a bit of a bummer especially because it's tied to the OS version.
But we have an (automatic) WebSocket fallback in the meantime.

### Connection Migration
**Bonus feature**: QUIC connections can survive IP address changes.
Switch from WiFi to cellular?
The stream keeps playing.

This is possible because QUIC uses connection IDs rather than the traditional `IP:port` tuple.
This also allows for some pretty neat [load balancing](https://datatracker.ietf.org/doc/html/draft-ietf-quic-load-balancers) techniques.

## Security
TLS 1.3 is required for QUIC.

This can be annoying for local development and private networks.
There is some performance overhead of course, but the main problem is that you need TLS certificates.

## See Also

- [moq-lite Layer](/concept/layer/moq-lite) - What we build on top of QUIC
- [Layer Overview](/concept/layer/) - How the layers fit together
