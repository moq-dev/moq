# [L] Back an internal OBS video encoder with moq-video

## Goal

One opt-in Use MoQ encoders choice publishes OBS video and audio through moq-video/moq-audio. OBS keeps composition, mixing, A/V timing, and the encoded output lifecycle. Existing OBS encoders remain selectable.

## Plan

- Register an internal OBS video encoder, backed by `moq_video::encode::Sink`. Retain `MoQOutput::EncodedPacket` and existing catalog handling. Do not replace the output with raw publication: OBS's encoded-output flag is output-wide, and bypassing it would duplicate A/V integration.
- Drive the codec-only `video.Encoder` that [Codecs](/quest/m1/ffi-shape/codec.md) adds, rather than adding an encoder type here (decided 2026-10-06 on #4519: Codecs owns `video.Encoder` beside the broadcast-bound `video.Producer`). It needs owned handles and packet draining, with no backend internals or caller cleanup callbacks; extend Codecs where it lacks them. `video.Producer` couples encoding to publication and is not the packet adapter.
- Start with H.264 by default and HEVC where supported. Keep reordering disabled; resolve Annex-B headers/decoder configuration, DTS/PTS and drain semantics explicitly, since Rust encoded output carries only timestamp, payload, and keyframe flag. Do not advertise unsupported AV1 encoding.
- Use the shared Low latency, Balanced, and Quality presets with bitrate separate, defaulting to Balanced: `moq_video::encode::Preset` in, `Applied` (the preset and controls that took effect) out for Stats. Neither reaches moq-ffi yet, so carry both through it and its bindings. Expose a single Use MoQ encoders option only once audio and video adapters both work. Retain the existing OBS encoder selection as an explicit alternative; do not silently switch back to OBS codecs after a MoQ codec failure.
- Establish bounded submission/packet queues, explicit raw-frame drop behavior, thread confinement, cancellation, late completion, device loss, resize and color metadata. Never block OBS's graphics thread on network backpressure. The CPU path is a correctness/fallback baseline; platform quests establish accelerated input.
- Test rejection, saturation, drain, stop during encode, delayed completion, and repeated start/stop. Validate real decoded pixels and audio continuity, matched timestamps, preset reporting, and frame-to-packet latency. Validate the new binding docs, feature combinations, package dependencies and native plugin linking.

## Required

- [C++ through moq-ffi](/quest/m1/cpp/README.md) - the plugin is on the generated C++ first
- [Audio publishing](/quest/m1/obs-moq-video/audio-publish.md) - both adapters are needed for the combined opt-in UI
- [Codecs](/quest/m1/ffi-shape/codec.md) - the encoder and decoder types land once, in the `audio` and `video` namespaces (decided in the 2026-10-05 audit)
