# [M] A hardware decoder's capability refusal is an unsupported config

## Goal

When every decoder that would take a codec refuses it because the device
can't decode that stream (for example an HEVC profile, a bit depth, or AV1 on
a GPU that lacks it), video subscribe reports `UnsupportedConfig`, not a
backend fault. A caller such as OBS can then fall back to another rendition,
as it can for codecs this build can't decode at all. OBS's audio already
falls back on `UnsupportedConfig` (`cpp/obs/src/moq-source.cpp`). Its video
still decodes with FFmpeg until [OBS native codecs](/quest/m2/obs-moq-video/README.md)
lands. Missing drivers, no device, and library faults stay faults. Done when
a capability refusal from each hardware backend that can detect one surfaces
as `MoqError::UnsupportedConfig` through moq-ffi and moq-c, and a probe-backed
test shows a mixed refusal and fault still reports a fault.

## Plan

Planned 2026-11-03 (maintainer, `/quest-plan`), from the follow-ups of
[moq-dev/moq#7730](https://github.com/moq-dev/moq/pull/7730), which typed
decoder setup refusals. That PR settled (✅, maintainer) that
`UnsupportedConfig` covers only a known-unsupported codec, layout, or
description, and that typed backend faults are never folded into it. This
quest keeps that rule: it makes a hardware backend know when it can't decode
a stream.

Today `select` in `rs/moq-video/src/decode/backend/mod.rs` returns
`Error::NoDecoder` (a fault) when every candidate that takes the codec fails
to open, whatever the reason. The consumer maps only `UnsupportedCodec` to
`UnsupportedConfig` (`rs/moq-video/src/decode/consumer.rs`).

- Give the backend open path a typed refusal, distinct from a fault, for a
  stream the device reports it can't decode. Each backend needs its own
  capability query or error code: VideoToolbox, VA-API (profile and
  entrypoint lists), NVDEC (decode caps), Media Foundation, MediaCodec, and
  V4L2. A backend that can't tell the two apart keeps reporting a fault.
- `select` reports `UnsupportedCodec` only when every tried candidate refused
  by capability. If any candidate faulted, the result stays `NoDecoder`.
  `Auto` with a working software fallback is unaffected.
- Test it with the probe decoders (`backend/probe.rs`), so it doesn't depend
  on the host GPU. Run the real-backend checks on whichever hosts have one,
  such as kixtop for VideoToolbox.

Public API: no new variants. More setups reach `UnsupportedConfig` instead of
`Video`. Note that in each binding's MIGRATE.md. Wire: none.

## Related

- [OBS native codecs](/quest/m2/obs-moq-video/README.md) - the OBS source whose rendition fallback this feeds
