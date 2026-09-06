# [S] Re-anchor SRT egress timestamps after a publisher rewind

## Goal

SRT egress timestamps resume from the new media timeline after a rewind instead
of mapping the rewound span into the past and clamping it to the first packet.

## Plan

`rs/moq-srt/src/server.rs` `serve_subscribe` paces `ts::Subscriber` output with
`moq_mux::Pacer` and clamps it with `clamp_to_floor`. It does not consume the
exporter's discontinuity counter. The exporter can restart its clock while this
caller retains its old anchor.

- Forward the exporter's discontinuity signal through `ts::Subscriber`.
- Re-anchor before assigning the first new generation's SRT send timestamp;
  preserve the receiver's zero-lead pacing and the first-packet floor.
- Flush any partial chunk with its existing timestamp before the new generation.
- Add a regression for a long rewind followed by successive grid slots, and
  exercise an SRT receiver across the transition. Keep ordinary reordered input
  and the 33-bit TS rollover continuous.

## Related

- [Rewind recovery PR](https://github.com/moq-dev/moq/pull/3375)
- [Controlled rewind evidence](https://github.com/moq-dev/moq/issues/2833#issuecomment-5554907607)
