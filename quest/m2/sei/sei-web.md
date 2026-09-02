# [S] Web SEI access

## Goal

The web Hang stack discovers `sei` sidecars and exposes their samples to
applications, so a browser can read timecode, ad markers, or telemetry without
subscribing to the video track at all.

## Plan

Add typed catalog bindings for the section and a subscriber that yields raw SEI
samples keyed by the video group sequence and frame ordinal they came from, the
exact identity the section defines, with the frame timestamp carried as data
for presentation-time sync. Keep it independent
of the decode path: nothing here feeds a `VideoDecoder`, because stripped video
decodes unchanged and WebCodecs does not surface SEI regardless.

Applications parse the NAL payloads themselves with whatever vocabulary they
need, so a new payload type requires no change here.

Test a sidecar-only subscriber with the video track never requested, a
subscriber that takes both, late join, and reconnect. Prove that a backgrounded
tab consuming only the sidecar draws no video bandwidth.

## Required

- [SEI section](/quest/m2/sei/sei.md) - defines the catalog and correlation
  contract
