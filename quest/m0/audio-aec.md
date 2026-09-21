# [M] Exclusive microphone ownership for echo cancellation

## Goal

One microphone owns an adaptive echo canceller. Shareable controls cannot be
used to attach the same adaptive state to another microphone, and creating a
second active reference does not silently disable the first.

## Plan

Engine::canceller replaces the engine's previous tap, and Canceller clones
share adaptive state despite the documented one-microphone restriction. Split
the exclusive attachment from a cloneable control handle. Refuse conflicting
attachments explicitly, with release on drop and a clear ownership path from
playback reference through capture teardown.

Keep the current audio-processing implementation. No multi-microphone mixing
or new processing backend is part of this change. Use fake capture/reference
sources to test a second attachment, replacement after drop, control clones,
and teardown while processing. Run the ownership regressions in CI without
real devices, retaining existing signal-quality tests separately.

Public API: AEC construction, capture attachment, and handle ownership change.
Wire: none. Update speakerphone examples and existing audio documentation.
