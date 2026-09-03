# [L] Room SDK

## Goal

A headless `@moq/room` package, extracted from hang.live's room layer:
announce-derived roster, local participant publishing, remote participant
composition, and the `hang/*.json` metadata convention, in the @moq signals
idiom. No service and no storage: joining a room is minting a moq-token rooted
at the room prefix, and the package docs show that seam (the LiveKit
AccessToken analogue). hang.live migrates onto the package as proof.

## Plan

- API sketch, ported from hang.live `app/src/room/{index,local,watch}.ts`
  with the minimal `preview/{room,member}.ts` pair as the starting point:
  - `Room`: input a connection (the URL and token root already carry the
    room prefix); runs the announce loop and exposes remote participants as
    a signal map keyed by identity, skipping the local paths.
  - `Local`: enable signals for camera/microphone/screen; publishes
    `<identity>/camera` (camera + mic, hd/sd renditions) and
    `<identity>/screen`, whose announce/unannounce is the screenshare
    lifecycle.
  - `Remote`: groups an identity's `camera` and `screen` broadcasts and
    composes watch + metadata into video/audio/user/presence signals.
  - Metadata: port `metadata.ts` (~330 lines, render-free). Core carries
    `user.json` (id, name, avatar) and the `preview.json` presence booleans;
    chat and location stay app-defined extensions of the same catalog
    section rather than SDK surface.
- Tokens: `root` = room prefix, `get: ""`, `put: "<identity>/"`, so
  participants cannot publish at each other's paths (hang.live grants `put`
  on the whole room subtree today). Document in the package README; no
  hosted endpoint.
- hang.live pins old @moq lines (net 0.3 as `@moq/lite`, publish 0.2, watch
  0.2); the extraction lands on the current packages, so migrating hang.live
  (in that repo) doubles as its overdue upgrade.
- This is the deliberate replacement for `<moq-meet>` (removed in moq#883 as
  a crude demo): a headless library, no element. Custom elements and
  framework bindings are follow-ups if wanted.

## Related

- [LiveKit client shim](/quest/m2/livekit-shim.md) - builds its
  Room/Participant facade on this
