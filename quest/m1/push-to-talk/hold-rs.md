# [M] Rust consumers hold a disabled rendition

## Goal

Every first-party Rust consumer keeps its selected rendition, and its
subscription, while the publisher has it disabled and no enabled alternative
exists, as [@moq/watch does](/quest/m1/push-to-talk/hold-js.md).

## Plan

Decided 2026-10-28 (maintainer): the hold applies in every first-party
consumer, with the same rule as the JS quest. Initial selection skips disabled
renditions, an enabled alternative wins, and a removed rendition is dropped.

Already holding: `moq_mux::select::Broadcast::retain` and `catalog::Select`,
`moq play`, moq-ffi's audio and video consumers, and moq-c's `moq_consume_*`.

Left:

- moq-gst `moqsrc`: `reconcile` (`rs/moq-gst/src/source/imp.rs`) stops a
  disabled rendition's reader and holds its pad idle. Keep the reader running
  while no rendition of its kind is enabled. The disable tests
  (`a_reenabled_rendition_resumes_on_the_same_pad`,
  `a_reenabled_sibling_rendition_plays_the_next_instance_after_a_finish`)
  change with it.
- Open: whether the exports hold too. `Source::retain_selectable`
  (`rs/moq-mux/src/source.rs`) drops a disabled rendition for every
  fMP4/TS/MKV/FLV/Annex B export, and the HLS export, WebRTC egress and RTMP
  play follow the flag on their own. A held track sends nothing, which an
  interleaving muxer or an HLS player may wait on, so the recommendation is
  that exports keep dropping it.

## Related

- [JS consumers hold a disabled rendition](/quest/m1/push-to-talk/hold-js.md) - the same rule in JS
- [OBS source picks renditions by the catalog contract](/quest/m2/obs-source-renditions.md) - holds through moq-c
