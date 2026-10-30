# [M] moq-rtc paces WHEP viewers to a cap, NACKs included

## Goal

An embedder can give a `moq-rtc` WHEP session a bitrate cap, and moq-rtc
never sends that viewer more media payload than the cap plus a burst,
retransmits included. The cap counts payload, like the
[QUIC caps](/quest/m2/rate-quic.md). RTP, SRTP, and UDP overhead sits
outside it: moq-rtc picks the packet sizes, so the overhead per payload
byte is fixed, and the viewer can't inflate it. This backs moq.pro's
egress caps.

## Plan

Decided 2026-10-20 (maintainer): planned upstream now, because moq.pro's
WHEP enforcement needs it and nobody owned it.

- Pace the RTP packets each frame packetizes into, charging their media
  payload, rather than whole frames: bucket credit caps at the burst, so a
  frame larger than the burst could never be released whole without
  exceeding cap plus burst. Decide drops only before a group starts: past
  cap plus burst, skip to the next group, the same whole-group drop
  moq.pro's RTMP play uses. A group that has started is paced through to
  its end even when it, or one of its frames, is larger than the burst, as
  large frames finish gradually on [QUIC](/quest/m2/rate-quic.md), so an
  oversized keyframe group still makes progress.
  Dropping individual RTP packets would leave partial frames that corrupt
  the picture and draw NACKs and PLIs nobody can satisfy.
- NACK resends draw from the same rate-plus-burst bucket as the frame pacer,
  and a resend with no credit left is skipped, so faking loss only spends the
  credit the viewer's next group would have used. str0m fulfils NACKs from
  its video send buffer (`rs/moq-rtc/src/session.rs`), so charging needs a
  hook on that path. Decided 2026-10-30 (maintainer): add it to the vendored str0m
  ([WebRTC epic](/quest/m2/webrtc/README.md)) rather than upstream.
- Decided 2026-10-30 (maintainer): the pacer is moq-congestion's, made public and
  extended with payload credit here, so moq-quic and moq-webrtc share one
  pacer; the vendored str0m's leaky pacer is deleted.
- A per-session rate handle sets the cap and accepts live updates. The pacer
  counts media; the docs advise headroom for RTP overhead.
- Tests: a capped viewer never receives more than cap plus burst, including
  a single frame larger than the burst, which still arrives; a viewer
  over the cap resumes cleanly on the next group; two viewers with different
  caps, each NACKing every packet for several burst windows, stay under
  their own cap plus burst.

Public API: a rate handle on moq-rtc's WHEP server. Wire: none.

## Required

- [Vendor and trim the stack](/quest/m2/webrtc/vendor.md) - the vendored str0m this adds the NACK hook to

## Related

- [moq-rtc REMB and TIAS](/quest/m2/rtc-remb-tias.md) - the WHIP half of the same caps
- [SRT rate handle](/quest/m2/srt-rate-handle.md) - the same pacing and retransmit charging on SRT
