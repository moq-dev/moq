# [L] LiveKit client shim

## Goal

A drop-in `livekit-client`-compatible JS package (e.g. `@moq/livekit`) that
runs a multi-participant room entirely over MoQ. The v1 surface is media plus
core events: Room connect/disconnect, local and remote participants,
camera/mic/screenshare publish, auto-subscribe, and the TrackSubscribed event
family; data surfaces (publishData, streams, RPC) are stubbed. The token slot
takes an ordinary moq-token, no LiveKit JWT parsing. Done when an
off-the-shelf LiveKit JS sample runs against a MoQ relay with only the import
and the connect URL/token changed.

## Plan

- The shim is a LiveKit-API facade over the
  [room SDK](/quest/m2/room-sdk.md), which carries hang.live's convention: the room is a path prefix in the connection URL and token root,
  participants are discovered from the bare announce stream, identity is the
  next path segment, and each participant publishes `<identity>/camera`
  (camera + mic, hd/sd renditions) and `<identity>/screen` (screenshare,
  whose announce/unannounce is the screenshare lifecycle). The shim groups
  the two paths per identity into one RemoteParticipant and maps catalog
  entries to TrackPublications.
- Build on `@moq/publish` and `@moq/watch`. LiveKit quality hints
  (setVideoQuality, adaptive settings) map to the receiver-driven pixel
  target, or no-op gracefully.
- v1 is identity-only: `participant.identity` comes from the path and muted
  state derives from catalog track presence. Names and coarse state are a
  follow-up wired to the room SDK's `hang/*.json` metadata, not a rival
  scheme.
- Recommend tokens scoped to `put: <identity>/` so participants cannot
  publish at each other's paths (hang.live grants `put` on the whole room
  subtree today).

## Required

- [Room SDK](/quest/m2/room-sdk.md)

## Related

- [WebRTC bridge evaluation](/quest/m3/livekit-webrtc-bridge.md)
