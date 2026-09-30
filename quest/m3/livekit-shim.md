# [L] LiveKit client shim

## Goal

A drop-in `livekit-client`-compatible JS package (e.g. `@moq/livekit`) that
runs a multi-participant room entirely over MoQ. The v1 surface is media plus
core events: Room connect/disconnect, local and remote participants,
camera/mic/screenshare publish, auto-subscribe, and the TrackSubscribed event
family; data surfaces (publishData, streams, RPC) are stubbed. The token slot
takes an ordinary moq-auth token, no LiveKit JWT parsing. Done when an
off-the-shelf LiveKit JS sample runs against a MoQ relay with only the import
and the connect URL/token changed.

## Plan

In m3 until a LiveKit user asks to migrate; `@moq/room` already serves
new rooms directly.

- The shim is a LiveKit-API facade over `@moq/room`, which carries hang.live's convention: the room is a path prefix in the connection URL and token root,
  participants are discovered from the bare announce stream, identity is the
  path before the broadcast name, and each participant publishes
  `<identity>/camera.hang` (camera + mic, hd/sd renditions) and
  `<identity>/screen.hang` (screenshare,
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
- Mint tokens with `@moq/room`'s `claims(room, identity)`, which scopes the
  `publish` claim to the identity's subtree so participants cannot publish at
  each other's paths.
