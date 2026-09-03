# [L] Reference vision worker

## Goal

A documented customer-run worker connects to a relay with an external processor
credential, subscribes to a low-resolution video rendition only when demanded,
runs object detection, and publishes frame-correlated bounding boxes as a Hang
contribution. The selected rendition is carried by the source broadcast rather
than referenced from another broadcast. A browser renders the boxes over
uninterrupted source video.

## Plan

Build the reference in an integration repository or in-tree example, not as a
hosted moq.pro (downstream) runtime. Keep the model replaceable and the output
contract plain: class, confidence, normalized bounds, source video rendition,
group sequence, and frame ordinal. The proof is about transport and lifecycle,
not model accuracy.

Provide a five minute local path with a synthetic or recorded source and a
second path against a deployed relay with a real publisher. Demonstrate no
source subscription before output demand, requested rendition selection,
bounded latest-frame processing, clean demand teardown, reconnect, two
interchangeable workers on one logical contribution using distinct
per-connection origin ids and the same source-derived groups and epoch, token
rotation, revocation, and source withdrawal. Exercise wildcard failover and
reconnect allocation without changing the contribution epoch. Republish at the
same source path with a new epoch and prove its contribution never splices onto
the old generation. Slow inference must drop stale work rather than build
latency. A fixture with a cross-broadcast rendition reference fails as
unsupported without subscribing to its target. A zero-epoch source remains
playable while the contribution refuses before the worker opens media.

Publish deployment examples for a local process and one generic container
platform, without tying anything in-tree to either. Record CPU/GPU and traffic
measurements so a customer can size its own worker.

## Required

- [Processor media contract](/quest/m2/processor/media-contract.md) - supplies
  the contribution and source-relation schema the example worker must publish
- moq.pro processor registration exists (downstream, external condition)
