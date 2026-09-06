---
title: MoQ vs HLS/DASH
description: Why MoQ beats segmented HTTP delivery on latency without giving up scale
---

# MoQ vs HLS/DASH

HLS and DASH won distribution because HTTP is stateless and cacheable, so
ordinary CDNs fan it out. Their weakness is latency: a few seconds for LL-HLS and LL-DASH on a good
network, and much more for conventional segments, with the exact figure set by
the encoder, CDN, and player.

## The problem

A conventional player downloads whole segments sequentially over TCP (the
low-latency variants split them into parts, which helps but keeps the shape). When the network degrades,
the current segment queues, the next one can't start, and the player can't
switch renditions until the boundary. Bufferbloat sets in, playback freezes,
and the player grows its buffer to avoid a repeat. Conventional segments are
also published only once complete, which is why smaller segments only help so
much.

## What MoQ does instead

Segments become groups, each on its own QUIC stream, streamed as they're
produced. When a new group starts during congestion it gets a higher priority
than the stalled one, so the player sees the fresh group arrive (possibly at a
lower rendition) while the old one starves. Past the viewer's latency budget,
the old group is skipped: a stutter and a jump forward, like conferencing,
instead of a buffering spinner. Audio outranks video, so sound stays continuous.

The viewer still controls the buffer. A subscription that asks for a 10 s
budget behaves like HLS; one that asks for 100 ms behaves like a video call.
Same protocol, same relay, same broadcast.

## Why not HTTP/3?

HTTP prioritization only works reliably on HTTP/2+ and only the client can
open a request, which makes publishing awkward. MoQ uses WebTransport to get
QUIC streams without HTTP semantics, but keeps HTTP's economics: an HTTP/3
CDN already runs a production QUIC stack, so adding MoQ is a small step. For
the devices that will never speak QUIC, hang carries CMAF and the
[HLS gateway](/bin/hls) serves any broadcast as HLS from the relay's cache.
