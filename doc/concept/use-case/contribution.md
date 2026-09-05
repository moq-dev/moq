---
title: MoQ vs RTMP/SRT
description: Pull-based contribution, on-demand encoding, and redundant ingest
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
transmission, and a publisher can go further and not encode either.

That matters for long-tail content: hundreds of security cameras uploading
360p, with 1080p pulled only when someone zooms in. It matters for AI too, where
a captions track backed by an expensive model runs only while someone has
captions on.

## Redundant ingest for free

Because tracks are only pulled where they're needed, a publisher can open
several connections that might be used. Primary and secondary ingest is two
connections and no business logic: subscriptions ride the primary until it
fails, then move. Two encoders sharing an origin id become interchangeable
sources that relays fail over between at a group boundary; see
[redundant publishers](/bin/cli#redundant-publishers).

## One protocol both ways

Contribution and distribution are the same problem with the arrows flipped:
client to server versus server to client, 1:1 versus 1:N. One protocol for
both means one implementation to optimize, one relay to deploy, and QUIC's
congestion control (this project's relay defaults to BBR) instead of a bespoke
UDP stack.

Existing encoders still work: the [OBS plugin](/bin/obs) publishes MoQ
directly, and [moq-cli](/bin/cli) accepts RTMP, SRT, and WHIP pushes.
