---
title: Layering
description: It's like a cake; you choose if you want frosting.
---

# Layers
The design philosophy of MoQ is to make things simple, composable, and customizable.
We don't want you to hit a brick wall if you deviate from the standard path (*ahem* WebRTC).
We also want to benefit from economies of scale (like HTTP), utilizing generic libraries and tools whenever possible.

To accomplish this, MoQ is broken into layers stacked on top of each other.
It's like a cake; you choose if you want frosting or not.


## QUIC
[QUIC](/concept/layer/quic) is the core protocol that powers HTTP/3, designed to fix the head-of-line blocking that plagues TCP.

Think of it like:
- **TCP 2.0**: connections with congestion control, flow control, and retransmissions. But now with independent streams and prioritization to avoid head-of-line blocking.
- **UDP 2.0**: optional reliability and datagrams, allowing stuff to get dropped during congestion.

It's a web standard, available in all major browsers, battle-tested by huge CDNs, and with great open-source implementations from every major tech company.

## WebTransport (optional)
[WebTransport](/concept/layer/web-transport) is a small layer that shares a QUIC connection with HTTP/3.

Basically it's like WebSocket but for QUIC instead of TCP.
Browsers need it because not using HTTP would be some cardinal sin or something.
Everybody else can use QUIC directly instead.

## WebSocket (optional)
[WebSocket](/concept/layer/web-socket) is a TCP fallback for when QUIC is blocked (corporate firewalls) or unsupported (Safari).

MoQ clients will automatically race the QUIC and WebSocket connections in parallel, using whatever wins the race.
It sucks but sometimes *media over QUIC* isn't actually an option.

## MoQ Transport
[moq-lite](/concept/layer/moq-lite) is a subset of the [MoqTransport](/concept/standard/moq-transport) specification.

The goal is a generic pub/sub protocol that can be scaled up via a CDN (see [moq-relay](/app/relay/)).
The CDN doesn't know anything about media, it just knows track/group/frame boundaries and what it should do during congestion.
Think of it like HTTP but for live content.

MoQ is bidirectional, so a **session** can be both a **publisher** and a **subscriber**:
- A publisher produces **broadcasts**, split into one or more **tracks**.
- A subscriber discovers available broadcasts and can choose to subscribe to tracks.

Each subscription is split into QUIC streams:
- A **group** is a QUIC stream (independent, unordered) that can be closed or reset (congestion).
- A **frame** is a chunk of bytes with an upfront size, delivered reliably and in order *within a group*.

## Media Format
[hang](/concept/layer/hang) is a simple media format running on top of moq-lite.
The relay doesn't care about media details, but the end clients need to agree on something: a catalog of tracks, a container for each frame, and codec configuration.

The IETF is working on a [suite of drafts](/concept/standard/msf) but the ideas are similar.

## Application
MoQ tracks are additive.
You can create new tracks for whatever purpose and it doesn't interfere with the function of a relay or media client.
Go ahead, create a `controller` track to stream button presses and it will be treated like any other opaque sequence of bytes.

MoQ is implemented in application space, not built into the browser like WebRTC.
If you don't like something, fork it and ship your own web and native apps.

## More Info
I'd recommend starting with the minimal:
- [moq-lite](/concept/layer/moq-lite)
- [hang](/concept/layer/hang)

Then read more about the various [IETF standards](/concept/standard/).
