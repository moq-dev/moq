# [L] Add VP8 and VP9 decoding to moq-video

## Goal

MoQ consumers, including the OBS source, decode VP8 and VP9 without depending on FFmpeg's ABI. This restores playback coverage deferred by the OBS decoder replacement; encoding is outside this quest.

## Plan

- Use vendored, statically linked libvpx as the initial portable decoder, preferring a maintained Rust binding over a bespoke wrapper. Its upstream SDK covers both VP8 and VP9. Pin a current stable release when implementing and validate licensing, packaged sources, supported profiles/bit depths, assembly tools, and Windows cross-build requirements. A dynamic FFmpeg wrapper does not meet the goal. See the [upstream build guide](https://chromium.googlesource.com/webm/libvpx/+/refs/heads/main/README).
- Integrate through moq-video's existing decode configuration and owned surface abstraction. Start with a portable software path; reuse native GPU output where available without making it a requirement for baseline support. Preserve timestamps, resolution changes, color metadata, keyframe recovery, and decoder reset behavior.
- Reconcile hang codec strings, existing VP8/VP9 mux metadata, catalog parsing, and C documentation. Unsupported profiles must produce explicit errors rather than decode incorrectly. Add fixture coverage across key/delta frames, corruption, resolution changes, and end-of-stream.
- Verify actual decoded pixels in Rust and the OBS source, CPU fallback, reconnect, and resource ownership. Inspect plugin/library imports and Cargo package contents to ensure no FFmpeg ABI dependency or undeclared system codec library was introduced.

## Related

- [Video source replacement](/quest/m2/obs-moq-video/source.md) - consumer integration, not a blocker for codec implementation
- [Color model](/quest/m2/color-model.md) - share codec-neutral color metadata rather than introducing VP9-only conversions
