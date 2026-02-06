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
