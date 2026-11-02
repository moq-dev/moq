# [M] moq-srt per-connection rate handle

## Goal

An embedder can give each `moq-srt` connection a bitrate cap through a rate
handle that stays valid across live rate updates. An SRT pull never sends
more media payload than the cap plus a burst, retransmits included, and an
SRT publish past the cap plus burst is closed with a reason the embedder can
log. The cap counts payload, as the [QUIC](/quest/m2/rate-quic.md) receive
cap does (its send cap counts whole ack-eliciting packets, QUIC overhead
included), but which payload depends on who muxes. On a pull, it is media payload: TS,
SRT, and UDP overhead sits outside it, because moq-srt picks the muxing and
packet sizes, so the overhead per payload byte is fixed and a viewer can't
inflate it. On a publish, it is the MPEG-TS payload the SRT socket yields,
TS headers, adaptation fields, and padding included, because the publisher
picks the muxing and could otherwise pad without limit. SRT and UDP
headers stay outside both. This backs
moq.pro's ingest and egress caps.

## Plan

Decided 2026-10-20 (maintainer): planned upstream now, because moq.pro's SRT
enforcement needs it and nobody owned it.

- Target the shared-socket integration that drives `srt-protocol` directly
  ([SRT on the shared socket](/quest/m2/one-port/srt-demux.md)). One port
  retires `srt-tokio` socket handling, so do not fork that backend to add
  this.
- Pull: pace the TS packets each frame muxes into, charging their media
  contribution, rather than whole frames, so a frame larger than the burst
  is released gradually instead of waiting forever or exceeding cap plus
  burst. Decide drops only before a group starts: past cap plus burst, skip
  to the next group; dropping 188-byte TS packets would leave partial
  frames. As in [moq-rtc pacing](/quest/m2/rtc-pacing.md), a group (or
  frame) larger than the burst is paced through to its end.
  Retransmissions draw from the same bucket, and one with no credit left is
  skipped, which needs the hook `srt-protocol`'s sender exposes when driven
  directly.
- Publish: SRT has no receiver backpressure, so meter the received TS bytes
  before demuxing and close the established session past cap plus burst. A
  CBR encoder's null packets count, so an embedder sizes the cap to the
  publisher's mux rate, not its media bitrate. An established SRT
  shutdown carries no reason, and `Forbidden` is a handshake rejection sent
  only before `accept`, so it stays for admission refusals; the rate-limit
  reason goes to the embedder instead.
- Burst defaults to about one second of the cap, as on QUIC.
- Tests: a capped pull stays under cap plus burst, including a single frame
  larger than the burst, which still arrives; two pulls with different
  caps, each reporting every packet lost for several burst windows, stay
  under their own; a publisher over the cap is closed mid-session, apart from
  a `Forbidden` admission refusal; a publisher whose TS mux rate, padding
  included, stays under the cap is untouched; under the cap nothing changes.

Public API: a rate handle and a rate-limit close reason on moq-srt's server.
Wire: none.

## Required

- [SRT on the shared socket](/quest/m2/one-port/srt-demux.md) - the direct `srt-protocol` integration this hooks into

## Related

- [moq-rtc pacing](/quest/m2/rtc-pacing.md) - the same pacing and retransmit charging on WHEP
- [moq-srt wire source](/quest/m2/wire-stats/srt.md) - per-connection byte counts from the same socket
