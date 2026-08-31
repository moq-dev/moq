# [L] Web SEI stitch

## Goal

The web Hang stack discovers `sei` sidecars, exposes raw sidecar samples to
applications, and can restore already-available SEI before feeding H.264/H.265
to a browser decoder. Missing or late SEI never delays video playback.

## Plan

Add typed catalog bindings and a reusable joiner in the JavaScript packages.
Keep the raw sidecar API independent of decoder support so captions,
telemetry, HDR, and vendor payload consumers can opt in without remuxing video.

Join by video group sequence and frame ordinal using the proven nonblocking
delivery contract, and bound unmatched state across skipped groups and
reconnects. Test WebCodecs and MSE paths that consume codec samples, plus a
direct sidecar subscriber.

The browser interoperability proof plays a separated-SEI stream under induced
sidecar delay and loss, verifies uninterrupted frame delivery, and confirms
that on-time NAL units retain byte order.

## Required

- [SEI section](/quest/m2/sei/sei.md) - defines the catalog and nonblocking
  correlation contract
- [Versioned SEI profile](/quest/m2/sei/sei-profile.md) - supplies the
  resolver boundary and old-client failure contract
- [Nonblocking SEI delivery](/quest/m3/sei-delivery.md) - proves the
  cross-track mechanism the browser implementation must use
