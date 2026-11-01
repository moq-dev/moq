---
title: MoQ vs RTMP/SRT
description: Pull-based contribution, optional on-demand encoding, and failover
---

# MoQ vs RTMP/SRT

Contribution protocols push: RTMP from OBS to Twitch, SRT from a studio
encoder, WHIP from a browser. Pushing means nothing is optional. A publisher
offering 360p and 1080p encodes and uploads both whether or not anything
downstream wants the second one.

## Pull changes the economics

A MoQ viewer's first act is subscribing to the catalog. It then subscribes to
the renditions it wants, and that subscription travels upstream (merging with
duplicates) until one copy reaches the publisher. No subscribers, no
transmission.

That matters for long-tail content: hundreds of security cameras uploading
360p, with 1080p pulled only when someone zooms in.

Encoding is a separate choice. MoQ's own publishers keep encoding video while a
source is enabled, and keep raw audio to encode once someone listens, so a
viewer who arrives late can still fetch the last few seconds. A publisher can
opt in to encoding on demand instead, which saves the work while nobody watches
at the cost of that past. It pays off when the encode is the expensive part,
such as a captions track backed by an AI model that runs only while someone has
captions on, or a transcoder's extra renditions.

## Failover and restarts

A broadcast can be announced over several connections. Seamless failover
between them needs a [stream instance](/concept/moq-lite#stream-instances)
carried on every hop (moq-lite 07, opt-in), which names the stream behind a
broadcast: replicas sharing one fail over mid-group, and a restart is a new
instance that viewers switch to instead of stalling on the old group numbers.
The RTMP, SRT, and WHIP ingests mint one per connection. Over an older wire the
receiving relay mints an instance per announcement, so failover onto such a
route is a restart.

## One protocol both ways

Contribution and distribution are the same problem with the arrows flipped:
client to server versus server to client, 1:1 versus 1:N. One protocol for
both means one implementation to optimize, one relay to deploy, and QUIC's
congestion control (this project's relay defaults to BBR) instead of a bespoke
UDP stack.

Existing encoders still work: the [OBS plugin](/bin/obs) publishes MoQ
directly, and [moq-cli](/bin/cli) accepts RTMP, SRT, and WHIP pushes.
