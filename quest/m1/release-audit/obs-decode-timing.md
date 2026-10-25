# [S] The OBS source keeps decoded timestamps and drains at EOF

## Goal

On the current FFmpeg path in `cpp/obs/src/moq-source.cpp`, each decoded
picture carries its own presentation timestamp, not the input packet's, and
a clean media EOF drains the decoder's delayed pictures. A reordered H.264
stream with B-frames plays every picture in order with correct timestamps.

## Plan

Split into m1 from the [OBS video source](/quest/m2/obs-moq-video/source.md)
replacement on 2026-10-22 (maintainer), so the cut does not ship the two
defects the release audit reproduced: `moq_source_decode_frame` assigns the
current packet's timestamp to an older reordered picture, and the clean EOF
callback returns without draining (about `moq-source.cpp:217` and `:634` at
1d43c096e0). The audit's libavcodec reproduction (12 B-frames, 10 pictures
all mis-stamped, two lost at EOF) is the regression test to automate.

Use the decoder's output timestamp (`best_effort_timestamp` or the frame's
`pts`). On clean EOF, send a flush packet and call `avcodec_receive_frame`
until it returns `AVERROR_EOF`, emitting every picture; the regression
asserts all 12 pictures and their timestamps. Keep the fix inside the
current path; the GPU replacement keeps the same behavior.

Public API: none. Wire: none.
