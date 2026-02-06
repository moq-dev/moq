---
title: MoQ vs HLS/DASH
description: How MoQ compares to distribution protocols like HLS/DASH
---

# MoQ vs HLS/DASH
This page compares MoQ with traditional **distribution protocols** like HLS and DASH.

## Requirements
Okay the boring stuff first.
Distribution protocols need to:

- Publish from a server to clients
- Scale to any number of clients
- Support a wide range of devices (especially web)
- Support a wide range of networks (especially mobile)
- Support live streaming (duh)
- (optional) support VOD and DVR
- (optional) support DRM 🤮

## Existing Protocols
- **HLS** ([HTTP Live Streaming](https://en.wikipedia.org/wiki/HTTP_Live_Streaming)) - Apple's protocol. Used to be required for iOS, now mainly for Airplay.
- **DASH** ([Dynamic Adaptive Streaming over HTTP](https://en.wikipedia.org/wiki/Dynamic_Adaptive_Streaming_over_HTTP)) - An MPEG standard that copies Apple but does it "better".
- **LL-HLS** ([Low-Latency HLS](https://en.wikipedia.org/wiki/HTTP_Live_Streaming#Low_Latency_HLS)) - A variant of HLS for lower latency.
- **LL-DASH** ([Low-Latency DASH](https://optiview.dolby.com/resources/blog/streaming/low-latency-dash/)) - Ditto.

The notable thing here is that HLS/DASH both use HTTP and benefit greatly from existing HTTP infrastructure.
A "state-less" protocol like HTTP is perfect for distribution because it can be cached and fanned out.
We're mostly going to be discussing HLS/DASH in the rest of this document.

Notable mentions:
- **RTMP** ([Real-Time Messaging Protocol](https://en.wikipedia.org/wiki/Real-Time_Messaging_Protocol)) - The classic Flash-era protocol before we switching to HTTP.
- **WebRTC** ([Web Real-Time Communication](https://en.wikipedia.org/wiki/WebRTC)) - Can be used for distribution, but it's not designed for it.

RTMP and WebRTC can technically offer a better user experience with lower latency, but they don't benefit from economies of scale.
A specialized RTMP/WebRTC CDN does not have the same level of optimization as a general-purpose HTTP CDN.

## Latency
Fun fact, I (`kixelated`) created MoQ because we hit the latency limits of HLS.
We were already using a `LHLS` variant for streaming frame-by-frame well before LL-HLS was a thing.

### The Problem
HLS/DASH are known for their high latency.
Even LL-HLS and LL-DASH can only achieve the ~2s-3s latency range on a good network.

The fundamental problem is how HLS/DASH respond to congestion.
The protocols work by downloading segments of media (ex. 2s) sequentially over HTTP.
When using HTTP/1 and HTTP/2, this means each segment of media is served over TCP.

When the network goes to shit, the current segment of media starts queuing, which also blocks the next segment from being served.
The queue builds up in a phenomenon not too different from [bufferbloat](https://en.wikipedia.org/wiki/Bufferbloat).
What's worse, the player can't\* really switch renditions until the next segment boundary, so it's stuck downloading the rest of the segment at an unsustainable bitrate.

\* Canceling the request would sever a HTTP/1 connection, requiring a slow TCP/TLS handshake. [HESP](https://en.wikipedia.org/wiki/High_Efficiency_Streaming_Protocol) is one work-around but it's complicated.

A HLS/DASH player uses a large buffer size so it can continue to play media even during these situations.
When the buffer is empty, playback will freeze and you get that dreaded "buffering" spinner.
Your latency will be higher after the buffer is refilled to help avoid this situation in the future.

It also doesn't help that segments are not streamed, which means a 2s segment sits on disk for 2s adding additional latency.
LL-HLS and LL-DASH address this by using smaller segments (~500ms), but still suffer from the same queuing problem.

### A Solution
MoQ solves this by using QUIC to stream segments in parallel.
And of course, segments are streamed instead of sitting on disk to further reduce latency.

During normal operation, the player will be near the live playhead and downloading the current segment.
When the network starts go to shit, the player will start to fall behind on the current segment like HLS/DASH.

However, when the next segment boundary is reached, the MoQ server will open a new QUIC stream for it.
This QUIC stream has a higher priority than the previous stream, so when bandwidth is limited, it will be served first.
To the player, the current segment will stall while the next segment (at a lower rendition) starts downloading.
If there's any bandwidth available, the current segment will make some progress but it (probably) won't be able to catch up.

Eventually, the player hits the maximum jitter buffer size and skips the current segment.
The user experience is like that of conferencing, where the old content might stutter before warping forward.

But note that audio is higher priority than video too.
There will be some desync during congestion events, but a continuous stream of audio helps smooth out the user experience.

### Dynamic Experience
While MoQ unlocks this new user experience, it won't always be desirable.
A user might *want* to buffer so they don't miss any content.

MoQ copies the HLS/DASH playbook and puts the viewer in control of the buffer size, and thus the target latency
The server will prioritize newer content only when indicated in the SUBSCRIBE request.
You can use the same protocol to deliver content with 100ms of latency or 10s of latency without a significant behavioral change.

Other protocols like WebRTC/SRT don't have this flexibility; the publisher decides when to drop content.
It doesn't matter how big the viewer's buffer is if WebRTC decides to drop after 100ms of congestion.
MoQ gives you the ability to distribution to a diverse set of viewers with different latency requirements.

### HTTP/3
One thing I want to mention is that MoQ does not use HTTP.
Ouch, I just said that HTTP was crucial for HLS/DASH.

One problem is that HTTP, by design, is version agnostic.
When you make a HTTP request, it could be using HTTP/1, HTTP/2, or HTTP/3.
Most of the time this doesn't matter, but it can put us in a bad situation unless we explicitly require a specific version.

For example, [prioritizing a HTTP request](https://developer.mozilla.org/en-US/docs/Web/HTTP/Reference/Headers/Priority):
- HTTP/1: Does nothing. TCP connections fight for bandwidth instead.
- HTTP/2: Prioritizes the new request, but data might still be stuck in the TCP queue.
- HTTP/3: Prioritizes the new request.

Any variant of MoQ over HTTP would have to require at least HTTP/2.
Otherwise, prioritizing a HTTP/1 request would be disastrous, as segments would fight for bandwidth rather than cooperate.
This is a solvable issue of course and I would love to see a protocol like HLS/DASH that copies MoQ's prioritization.

The main reason why we use WebTransport is just to avoid HTTP semantics.
Only the HTTP client can issue a HTTP request and there's no way (any longer) for the server to push content to the client.

We could totally have the client increment a sequence number (like HLS/DASH) and constantly request the next segment.
But then when doing [contribution](/concept/use-case/contribution/), the client/server dynamic is inverted.
Rather than fight HTTP semantics, we're using a proper bidirectional protocol like QUIC/WebTransport instead.

### Why it doesn't Matter
Remember that economies of scale are gud.
We can still get the benefits of HTTP without using HTTP.

First off, we did the next best thing and based the protocol on QUIC.
There's a production-grade QUIC library behind every HTTP/3 implementation.
Any CDNs that support HTTP/3 have the capability to slot MoQ support in with minimal effort.

Second off, the [MoqTransport](/concept/standard/moq-transport) layer is generic.
It's an ambitious attempt to surplant HTTP for live content, not just media content.
More use-cases means more emdna which means more customers which means more investment which means you get the idea.

### Device Support
The biggest uphill battle for MoQ is device support.

HTTP has powered the internet for decades and it's not going anywhere anytime soon.
HTTP/3 is a new kid on the block, and MoQ can ride that momentum, but it's going to take years before every device supports QUIC.

I think the most crucial feature for MoQ is backwards compatibility with HLS/DASH.
The ability to serve HLS/DASH content via MoQ and HTTP is necessary for the industry to adopt MoQ.

That's why [hang](/concept/layer/hang) can support CMAF and [moq-relay](/app/relay/) can serve tracks via HTTP.
More documentation coming soon.
