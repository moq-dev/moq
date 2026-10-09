# [M] OBS multitrack video as simulcast renditions

## Goal

The obs-moq output publishes OBS's native multitrack video encoders as
simulcast renditions of one broadcast, one rendition per track, each with
its own size and bitrate in the catalog. A user configures them the way OBS
multitrack video is configured, including over obs-websocket, and the top
renditions disable as the bandwidth grant falls (see
[simulcast rung disable](/quest/m1/simulcast-rung-disable.md)).

## Plan

Verify the gap first. The output already declares
`OBS_OUTPUT_MULTI_TRACK_VIDEO` and keeps one track per video encoder
(`video_tracks` in `cpp/obs/src/moq-output.cpp`), so the work is likely
configuration, naming, and catalog correctness rather than new plumbing.
moq-mux already imports enhanced-RTMP multitrack
(`rs/moq-mux/src/container/flv/import.rs`); match how it names and describes
renditions so an OBS stream looks the same whether it arrives over MoQ or
RTMP.

- Reuse OBS's own multitrack settings rather than adding a MoQ-only ladder
  UI, so obs-websocket and existing profiles drive it unchanged. Find out
  what OBS needs from a custom service to enable multitrack and record it.
- Each rendition advertises its real encoded size, codec, and maximum
  bitrate; disabling one never removes it from the catalog.
- Update `doc/bin/obs.md`, whose "OBS publishes one rendition" section this
  changes.

Verify with `just obs compile` and `just obs test`, and one manual run with
at least two video tracks watched in `<moq-watch>`.

## Related

- [Simulcast rung disable](/quest/m1/simulcast-rung-disable.md) - the grant-driven rung disable these renditions follow
- [MoQ in obs-studio](/quest/m1/obs-studio/README.md) - the native output this configuration carries into
- [OBS native codecs](/quest/m1/obs-moq-video/README.md) - the encoders behind each track when MoQ encoders are chosen
