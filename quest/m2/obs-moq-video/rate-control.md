# [S] OBS follows its bandwidth grant

## Goal

The OBS plugin's video output follows the connection's bandwidth share: it
reserves the Quality tab's bitrate for the video track through libmoq, reads
the grant before each encoded frame, and retunes the OBS encoder when the grant
moves, with the configured bitrate as the ceiling. Audio reserves its bitrate
and never follows. The dock's Stream stats show the current target beside the
configured rate.

## Plan

Uses the reservation surface from libmoq (`moq_session_bandwidth`,
`moq_bandwidth_reserve`, `moq_reservation_grant`). Apply grants through
the shape `moq_video::encode::rate::Control` uses (drops at once, raises ramp,
hysteresis) rather than pushing every change into `obs_encoder_update`; whether
that policy sits in libmoq behind the reservation or in the plugin depends on
whether a second C consumer wants it. Verify against a shaped uplink and with
`just obs compile` and `just obs test`; document the behaviour in
`doc/bin/obs.md`.

