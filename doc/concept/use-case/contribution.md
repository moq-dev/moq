---
title: MoQ vs RTMP/SRT
description: How MoQ compares to contribution protocols like RTMP and SRT
---

# MoQ vs RTMP/SRT
This page compares MoQ with traditional **contribution protocols** like RTMP and SRT.

## Requirements
Okay the boring stuff first.
Contribution protocols need to:

- Publish from a client to a server
- Interface with encoders and other media sources (like OBS)
- Support a wide range of devices (browsers optional)
- Support a wide range of networks (especially mobile)
- Support the latest and greatest codecs
- Support live streaming (duh)
- (optional) support ad signaling 🤮

## Existing Protocols
- **RTMP** ([Real-Time Messaging Protocol](https://en.wikipedia.org/wiki/Real-Time_Messaging_Protocol)) - The classic Flash-era protocol
- **SRT** ([Secure Reliable Transport](https://en.wikipedia.org/wiki/Secure_Reliable_Transport)) - Modern "low-latency" alternative
- **E-RTMP** ([Enhanced RTMP](https://en.wikipedia.org/wiki/Real-Time_Messaging_Protocol#Enhanced_RTMP)) - Modernized version of RTMP
- **WebRTC** ([Web Real-Time Communication](https://en.wikipedia.org/wiki/WebRTC)) - Can be used for contribution via [WHIP](https://www.rfc-editor.org/rfc/rfc9725.html)
- **RTSP** ([Real-Time Streaming Protocol](https://en.wikipedia.org/wiki/Real-Time_Streaming_Protocol)) - Used in IP cameras

The contribution landscape is quite fragmented, mostly split into two camps:
1. User generated content (YouTube/Twitch/Facebook) primarily uses RTMP.
2. Studio generated content primarily uses SRT.

That's an over-generalization of course, but it's very interesting to see the divide.
SRT is built into modern production equipment (hardware) while RTMP is used in consumer software.

Why? IDK.

## Pull vs Push
Existing contribution protocols are push-based.
Even Youtube's weird HLS ingest thing operates via POST requests.

However, MoQ is fundamentally a pull-based protocol.
Technically, MoqTransport supports push too (via PUBLISH), but hear me out for a second.

### The Push Problem
I would say there is one major problem with push: **There's no "optional" content.**

When a publisher creates multiple tracks, like 360p and 1080p, it needs to simultaneously encode and transmit both tracks.
There's no way of knowing if anything downstream *actually* wants the 1080p track; it might go straight to `/dev/null` on the media server.

This doesn't matter for huge events like a concert or sports game.
With enough viewers, we can assume that at least one viewer will want the content.
But it can be a significant cost for long-tail content that nobody watches.

For example, consider a facility with hundreds of security cameras.
We might be able to afford uploading 360p for every camera (recording to disk), but anything more than that would over-saturate the network.
Ideally, we could only stream 1080p from individual cameras when a human wants a closer look...

### The Pull Solution
The first thing a MoQ viewer does is subscribe to the `catalog.json` track for a broadcast.
This lists all of the available tracks and their properties.

If a viewer wants the 1080p track, it subscribes to it.
The subscription makes its way upstream (combining with duplicates) until one subscription reaches the publisher.
When no more viewers want the 1080p track, the subscription is cancelled.

The publisher won't transmit a track until there's an active subscription, saving bandwidth.
The publisher can go the extra mile and not even encode the content without a subscription, saving compute.
This is especially useful for expensive AI models, for example only running whisper when captions are needed.

Note that media services can also benefit from the same behavior.
If nobody currently wants the 1080p track, then don't transcode it.
The "publisher" in this case is any entity that understands the media format on top of MoQ.

## Multiple Connections
Another issue with push-based protocols is that each connection is expensive.
If every connection needs its own copy of the content, we quickly run out of bandwidth.
Redundant ingest is mostly limited to large events that have bandwidth to spare (active-active).

Once again, MoQ solves this via the pull model.
A publisher can establish multiple connections that *might* be used.
A subscription will only be issued if the connection needs a specific track.

For example, a service can implement primary/secondary ingest via two connections to separate endpoints.
All subscriptions are issued over the primary connection but if it fails, the subscriptions are moved to the secondary connection.
The endpoints don't even have to be part of the same CDN and MoQ publisher is completely oblivious; it just knows it was told to connect to two URLs.

Another example is P2P streaming.
A client can establish a connection to each peer, transmitting tracks as requested.
If one peer has the video minimized, then it can unsubscribe from the video track and save bandwidth.
Again there's no business logic for this built into MoQ: it's automatic.

But what about clients that don't support P2P?
Each client can also establish a connection to a MoQ CDN as a fallback.
This works because the client discovers all available broadcasts available on a connection via the built-in [announce mechanism](/feature/announce).
If two connections can serve the same content, the subscription goes to the "best" connection (ie. P2P > CDN).

## Economies of Scale
A subtle problem with contribution protocols is that they're not used for distribution.

This might silly: "of course distribution and contribution are different!"
But when you really sit down and break down the requirements, they're not that different.
One is client-server while the other is server-client, one is 1:1 while the other is 1:N.

By designing a protocol that works for both contribution and distribution, we can share implementations and optimizations.
There are other benefits of supporting 1:N too, as mentioned in the previous section, so it seems like a no-brainer.

The other way we benefit from economies of scale is by using QUIC.
We're not implementing our own UDP-based protocol and rediscovering the rough edges of the internet all over again.
A QUIC library with BBR will out-perform the system TCP stack and likely out-perform any custom UDP thing (ex. SRT).
