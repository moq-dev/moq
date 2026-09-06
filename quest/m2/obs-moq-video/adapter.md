# [L] Back an OBS video encoder with moq-video

## Goal

An opt-in MoQ video encoder publishes OBS's composited scene with working audio, timestamps, keyframes, bitrate control, and an explicit latency policy. A tested ownership contract supports later native GPU frames.

## Plan

- Prototype the OBS encoder adapter versus raw output. Prefer the adapter to preserve OBS audio encoding, `MoQOutput::EncodedPacket`, and existing catalog handling. Document the packet format, decoder configuration, DTS/PTS and drain requirements that make the choice work.
- Trace `rs/moq-video/src/encode` and `rs/libmoq/src/video.rs`. Reuse the existing encoder and surface types. If C needs a codec-only interface, return owned handles and poll/drain packets; do not expose Rust backend internals or caller cleanup callbacks. Keep the existing publishing API intact. Public ABI additions need C docs and consumer coverage.
- Specify submission ownership on success and rejection, packet release, bounded in-flight frames, cancellation, late completion, device loss, resize, color range/primaries, and audio/video timestamp conversion. Never block OBS's graphics thread on network backpressure. A full queue must have an explicit frame-drop policy, preserving decoder dependencies.
- Build a CPU I420/RGBA baseline for correctness and measurement. Preserve the OBS encoder path as an explicit fallback, and expose the actual encoder and input path in Stats. Do not present CPU upload as GPU zero-copy.
- Measure scene timestamp to encoded packet p50/p95 latency and queue depth, CPU/GPU utilization, throughput, and dropped frames against the current OBS encoder at matched codec, resolution, rate, and bitrate. Separate codec buffering from keyframe join delay, network delivery, and viewer playout.
- Add deterministic tests for rejection, saturation, drain, stop during encode, delayed completion, and repeated start/stop. Run a real subscriber and verify decoded pixels, audio continuity, and timestamps, rather than accepting packet production alone.
- Keep codec/latency compatibility explicit. An unavailable requested backend must not silently become a high-latency encoder. Validate `cargo package` for any new library dependency, feature combinations, and plugin linking on the supported platforms.

## Related

- [OBS callback lifetime](/quest/m0/obs-session-callback-lifetime.md) - coordinate terminal ownership without depending on a two-second timeout
