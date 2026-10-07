---
title: MoQ vs RTMP/SRT
description: Pull-based contribution, on-demand encoding, and restarts that take the name
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

## Restarts take the name

A publisher announces an [epoch](/concept/moq-lite#publisher-epochs) naming the
instance behind a broadcast. Replicas of that instance, announced with the same
epoch, fail over mid-group. A restart mints a new epoch, and viewers switch to
it instead of stitching its group numbers onto the old run. `moq` and
`moqsink` mint a fresh epoch per run. A second process publishing the same name
is a newer epoch, so viewers move to it.

## One protocol both ways

Contribution and distribution are the same problem with the arrows flipped:
client to server versus server to client, 1:1 versus 1:N. One protocol for
both means one implementation to optimize, one relay to deploy, and QUIC's
congestion control (this project's relay defaults to BBR) instead of a bespoke
UDP stack.

Existing encoders still work: the [OBS plugin](/bin/obs) publishes MoQ
directly, and [moq-cli](/bin/cli) accepts RTMP, SRT, and WHIP pushes.
