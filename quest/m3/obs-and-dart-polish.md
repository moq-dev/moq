# [S] Fix OBS token-refusal races, empty-frame EOF, and Dart QuicConfig option

## Goal

OBS output token refusals ignore stale callbacks from prior epochs and clear
prior refusal state on acceptance, OBS source skips zero-byte frames instead of
flushing FFmpeg with an EOF packet, and Dart `ListenOptions` exposes
`QuicConfig`.

## Plan

Decided 2026-11-21 (maintainer, /quest-plan of the exhaustive pre-cut
full-codebase audit): ✅ bundle these real low-severity OBS and Dart binding
bugs into a compact `m3` quest (rejected: dropping low-severity bugs; also
dropped `moq-boy` and `moq-ffi` internal lock refactors).

1. **`cpp/obs/src/moq-output.cpp:631-667`**: track a generation counter for
   `PresentToken` refusal callbacks so a late refusal from a superseded
   session cannot overwrite current state, and clear `token_refusal` when a
   token is accepted.
2. **`cpp/obs/src/moq-source.cpp:1223-1231`**: return `Sent::Accepted` early
   when a received frame has zero bytes instead of passing an empty
   `AVPacket{nullptr, 0}` to `avcodec_send_packet`, which signals decoder EOF.
3. **`dart/moq/lib/src/server.dart:11-52,95-98`**: expose `QuicConfig` on
   `ListenOptions` and forward it into the underlying FFI listen config for
   parity with the other bindings.

Public API: additive optional `quic` field on Dart `ListenOptions`. Wire: none.
Add unit tests covering OBS empty-frame handling / stale refusal callbacks and
Dart `ListenOptions` forwarding.
