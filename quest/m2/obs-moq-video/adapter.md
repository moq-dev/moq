# [L] Back an internal OBS video encoder with moq-video

## Goal

One opt-in Use MoQ encoders choice publishes OBS video and audio through moq-video/moq-audio. OBS keeps composition, mixing, A/V timing, and the encoded output lifecycle. Existing OBS encoders remain selectable.

## Plan

- Register an internal OBS video encoder, backed by `moq_video::encode::Sink`. Retain `MoQOutput::EncodedPacket` and existing catalog handling. Do not replace the output with raw publication: OBS's encoded-output flag is output-wide, and bypassing it would duplicate A/V integration.
- Add a clean codec-only C boundary with owned handles and packet draining, extending shared primitives from the audio adapter where appropriate. Avoid backend internals and caller cleanup callbacks. The existing raw-video publishing API couples encoding to publication and is not the packet adapter.
- Start with H.264 by default and HEVC where supported. Keep reordering disabled; resolve OBS keyframe flags, Annex-B headers/decoder configuration, DTS/PTS and drain semantics explicitly, since Rust encoded output currently contains only timestamp and payload. Do not advertise unsupported AV1 encoding.
- Use the shared Low latency, Balanced, and Quality presets with bitrate separate. Expose a single Use MoQ encoders option only once audio and video adapters both work. Retain the existing OBS encoder selection as an explicit alternative; do not silently switch back to OBS codecs after a MoQ codec failure.
- Establish bounded submission/packet queues, explicit raw-frame drop behavior, thread confinement, cancellation, late completion, device loss, resize and color metadata. Never block OBS's graphics thread on network backpressure. The CPU path is a correctness/fallback baseline; platform quests establish accelerated input.
- Test rejection, saturation, drain, stop during encode, delayed completion, and repeated start/stop. Validate real decoded pixels and audio continuity, matched timestamps, preset reporting, and frame-to-packet latency. Validate new public C docs, feature combinations, package dependencies and native plugin linking.

## Required

- [Encoder presets](/quest/m2/obs-moq-video/presets.md) - common policy
- [Audio publishing](/quest/m2/obs-moq-video/audio-publish.md) - both adapters are needed for the combined opt-in UI

## Related

- [OBS callback lifetime](/quest/m0/obs-session-callback-lifetime.md) - preserve output ownership through delayed completion
