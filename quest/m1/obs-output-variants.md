# [XS] Drop the unused OBS audio-only and video-only outputs

## Goal

The OBS plugin registers only `moq_output`. The `moq_output_audio` and
`moq_output_video` variants are gone.

## Plan

Reproduced in the 2026-10-22 binding and OBS audit at main
[1f5caa7026](https://github.com/moq-dev/moq/commit/1f5caa7026).
`register_moq_output` in `cpp/obs/src/moq-output.cpp` registers both variants
on top of `MoQOutput`. `MoQOutput::Start()` always requires a video encoder,
so `moq_output_audio` fails with "Failed to get video encoder" before capture
begins. Nothing in-tree creates either variant: the service's
`get_output_type` and the dock both use `moq_output`.

Decided with the maintainer on 2026-10-22: delete both variants instead of
validating encoders per variant. Neither has an in-tree consumer, and the
audio-only one has never worked. An obs-websocket or script user could name
`moq_output_video` by id. That removal is breaking for them, so call it out
in the PR and the plugin's release notes, and point them to `moq_output`.
