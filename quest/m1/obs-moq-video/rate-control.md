# [S] OBS follows its bandwidth grant

## Goal

The OBS plugin's video output follows the connection's bandwidth share: it
reserves the Quality tab's bitrate for the video track through the generated
C++ package, reads
the grant before each encoded frame, and retunes the OBS encoder when the grant
moves, with the configured bitrate as the ceiling. Audio reserves its bitrate
and never follows. The dock's Stream stats show the current target beside the
configured rate.

## Plan

Uses the moq-ffi reservation surface through the generated C++ package
(`MoqSession::bandwidth`, `MoqBandwidth::reserve`, `MoqReservation::grant`),
not the hand-written moq-c, since the plugin moves off it first. Apply grants through
the shape `moq_mux::rate::Control` uses (drops at once, raises ramp,
hysteresis) rather than pushing every change into `obs_encoder_update`; whether
that policy sits in moq-ffi behind the reservation or in the plugin depends on
whether a second binding wants it. Verify against a shaped uplink and with
`just obs compile` and `just obs test`; document the behaviour in
`doc/bin/obs.md`.

## Required

- [C++ through moq-ffi](/quest/m1/cpp/README.md) - the plugin is on the generated C++ first

## Related

- [Audio follows the grant](/quest/m1/2848-follow-the-bandwidth-grant-in-moq-audio-instead-of.md) - the shared rate policy audio adopts; OBS audio still reserves only
