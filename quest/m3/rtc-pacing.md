# [M] moq-webrtc paces WHEP viewers to a cap, NACKs included

## Goal

An embedder can give a `moq-webrtc` WHEP session a bitrate cap, and moq-webrtc
never sends that viewer more media payload than the cap plus a burst,
retransmits included. The cap counts payload, unlike the
QUIC send cap (`moq_quic::Limits::send_rate`), which counts whole ack-eliciting
packets, QUIC overhead included. RTP, SRTP, and UDP overhead sits outside
it: moq-webrtc picks the packet sizes, so the overhead per payload byte is
fixed, and the viewer can't inflate it. This backs moq.pro's
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
  large frames finish gradually on QUIC, so an
  oversized keyframe group still makes progress.
  Dropping individual RTP packets would leave partial frames that corrupt
  the picture and draw NACKs and PLIs nobody can satisfy.
- NACK resends draw from the same rate-plus-burst bucket as the frame pacer,
  and a resend with no credit left is skipped, so faking loss only spends the
  credit the viewer's next group would have used. str0m fulfils NACKs from
  its video send buffer (`rs/moq-webrtc/src/session.rs`), so charging needs a
  hook on that path. Decided 2026-10-30 (maintainer): add it to the vendored str0m
  ([WebRTC epic](/quest/m3/webrtc/README.md)) rather than upstream.
  Changed 2026-11-16 (maintainer, /quest-plan of the pre-cut audit):
  moq-webrtc [returns to upstream str0m](https://github.com/moq-dev/moq/pull/8963), so
  there is no vendored copy to add the hook to. Start by finding out
  whether str0m 0.25's public API can charge or withhold a resend (its RTP
  mode, or turning off its own NACK handling and serving resends from a
  buffer moq-webrtc keeps). If not, the hook is a change inside str0m:
  write the report for [upstream reports](/quest/m4/upstream-reports.md).
  If the cap cannot wait for upstream, bring the evidence to the maintainer,
  who decides whether this fix is the one worth a fork.
- Decided 2026-10-30 (maintainer): the pacer is moq-congestion's, made public and
  extended with payload credit here, so moq-quic and moq-webrtc share one
  pacer. str0m's own leaky pacer stays unused, as it is today.
- A per-session rate handle sets the cap and accepts live updates. The pacer
  counts media; the docs advise headroom for RTP overhead.
- WHIP ingest follow-ups (2026-10-31, from
  [moq-dev/moq#7373](https://github.com/moq-dev/moq/pull/7373), which
  meters a capped WHIP publisher's frame payload): it does not meter
  inbound RTP that never forms a frame, so a hostile publisher can send junk
  RTP past its cap. Meter it with the same bucket here if that proves to
  matter, without counting BWE padding probes (metering raw SRTP bytes would
  trip an honest Chrome while probing). #7373 was tested against str0m on
  both ends only; check once with a real Chrome or Firefox publisher that
  it honors `b=TIAS` and REMB beside transport-cc, stays under the meter
  through keyframes and BWE probing at 90% of the cap, and lifts its
  ceiling on a `Bitrate::MAX` REMB. Do the same real-browser check for the
  WHEP cap this quest adds.
- Tests: a capped viewer never receives more than cap plus burst, including
  a single frame larger than the burst, which still arrives; a viewer
  over the cap resumes cleanly on the next group; two viewers with different
  caps, each NACKing every packet for several burst windows, stay under
  their own cap plus burst.

Public API: a rate handle on moq-webrtc's WHEP server. Decided 2026-10-30
(maintainer): `whep::accept` takes the same shape as `whip::accept`, a
`whep::Request` struct, rather than a fifth argument. That struct exists,
`#[non_exhaustive]`, so this quest adds a `rate: Option<bandwidth::Limit>`
field to it, never a
`with_rate` setter (2026-11-07, maintainer, /quest-plan from the API sweep
of open PRs and remaining quests). `whip::Request` had the same field until
[the gateway alignment](/quest/m2/gateway-align.md) moved the ingest cap
onto `moq_mux::Ingest` (2026-11-16); the WHEP cap is per viewer, so it
stays on the request. The handle
is `moq_net::bandwidth::Limit`, which replaced moq-webrtc's own
`rate::{Cap, Limit}` (2026-11-07, maintainer, /quest-plan from the API shape
audit). Wire: none.

## Related

- [SRT rate handle](/quest/m3/srt-rate-handle.md) - the same pacing and retransmit charging on SRT
