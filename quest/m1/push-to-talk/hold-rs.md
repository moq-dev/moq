# [M] moqsrc holds a disabled rendition

## Goal

Every first-party Rust player keeps its selected rendition, and its
subscription, while the publisher has it disabled and no enabled alternative
exists, as `@moq/watch` does. Exports keep dropping a disabled rendition.

## Plan

Decided 2026-10-28 (maintainer): the hold applies in every first-party player.
The rule matches `@moq/watch`: initial selection skips disabled renditions, an
enabled alternative wins, and a removed rendition is dropped.

Already holding: `moq_mux::select::Broadcast::retain` and `catalog::Select`,
`moq play`, moq-ffi's audio and video consumers, and moq-c's `moq_consume_*`.

Left: moq-gst `moqsrc`. `reconcile` (`rs/moq-gst/src/source/imp.rs`) stops a
disabled rendition's reader and holds its pad idle. Keep the reader running
while no rendition of its kind is enabled. The disable tests
(`a_reenabled_rendition_resumes_on_the_same_pad`,
`a_reenabled_sibling_rendition_plays_the_next_instance_after_a_finish`)
change with it.

Decided 2026-10-28 (maintainer): exports do not hold. That covers every
fMP4/TS/MKV/FLV/Annex B export through `Source::retain_selectable`
(`rs/moq-mux/src/source.rs`), plus the HLS export, WebRTC egress and RTMP
play. A held track sends nothing, so an interleaving muxer or an HLS player
may wait on it, and an export has no unmute latency at stake the way a
listener does.

## Related

- [OBS source picks renditions by the catalog contract](/quest/m2/obs-source-renditions.md) - holds through moq-c
