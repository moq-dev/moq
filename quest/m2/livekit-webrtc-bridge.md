# [M] LiveKit WebRTC bridge evaluation

## Goal

A go/no-go verdict, backed by a working spike, on bridging LiveKit rooms to
MoQ per-track over WebRTC using LiveKit's Rust SDK: a native participant
joins the room and republishes its tracks into MoQ, optionally the reverse.
The verdict decides whether this becomes a maintained gateway.

## Plan

- The zero-code paths already exist and bound the value: LiveKit Egress
  pushes RTMP/SRT into the existing gateways today (composited and
  transcoded), and `moq export rtc --connect` publishes WHIP into LiveKit
  Ingress. LiveKit has no WHIP/WHEP egress, so per-track LiveKit to MoQ
  requires joining the room as a participant via livekit rust-sdks
  (Apache 2.0, no Chrome/GStreamer).
- Spike it in-tree: a rust-sdks participant subscribes to all tracks and
  republishes via moq-net + moq-mux, with codec passthrough where possible
  (Opus/VP8/VP9; H.264 through the Annex-B importer).
- Assess dependency weight of the webrtc stack, per-track fidelity versus the
  Egress paths, simulcast handling, and where a real gateway would live (edge
  versus standalone).

## Related

- [LiveKit client shim](/quest/m2/livekit-shim.md)
