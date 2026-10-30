# [S] moq-quic pads in one frame

## Goal

moq-quic encodes a run of padding as one padding frame, not one
`Frame::Padding` per byte, so padding a packet costs time proportional to the
frames sent, not to the bytes padded.

## Plan

Found by [moq-dev/moq#6987](https://github.com/moq-dev/moq/pull/6987), where
per-byte padding frames showed up in test CPU. It is a per-packet cost in
production too. Padding bytes are all zero on the wire, so this changes only
how we represent and encode them, not what we send.

Decided 2026-10-30 (maintainer, /quest-plan of the 2026-10-30 /quest-complete follow-ups): ✅ m1 (picked over the recommended m2 perf).

Public API: none. Wire: none.
