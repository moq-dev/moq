# [M] moq-srt per-connection rate handle

## Goal

An embedder can give each `moq-srt` connection a bitrate cap through a rate
handle that stays valid across live rate updates. An SRT pull never sends
more media payload than the cap plus a burst, retransmits included, and an
SRT publish past the cap plus burst is closed with a reason the embedder can
log. The cap counts payload, like the [QUIC caps](/quest/m2/rate-quic.md).
TS, SRT, and UDP overhead sits outside it: moq-srt picks the muxing and
packet sizes, so the overhead per payload byte is fixed, and a viewer can't
inflate it. This backs
moq.pro's
[ingest](https://github.com/moq-dev/moq.pro/blob/main/quest/m2/rate-ingest.md)
and [egress](https://github.com/moq-dev/moq.pro/blob/main/quest/m2/rate-egress.md)
caps.

## Plan

Decided 2026-10-20 (maintainer): planned upstream now, because moq.pro's SRT
enforcement needs it and nobody owned it.

- Target the shared-socket integration that drives `srt-protocol` directly
  ([SRT on the shared socket](/quest/m2/one-port/srt-demux.md)). One port
  retires `srt-tokio` socket handling, so do not fork that backend to add
  this.
- Pull: pace frames to the cap before muxing TS. Past cap plus burst, skip to
  the next group; dropping 188-byte TS packets would leave partial frames.
  As in [moq-rtc pacing](/quest/m2/rtc-pacing.md), skip only at a group
  boundary, so a group larger than the burst is paced through to its end.
  Retransmissions draw from the same bucket, and one with no credit left is
  skipped, which needs the hook `srt-protocol`'s sender exposes when driven
  directly.
- Publish: SRT has no receiver backpressure, so meter received bytes and
  close the established session past cap plus burst. An established SRT
  shutdown carries no reason, and `Forbidden` is a handshake rejection sent
  only before `accept`, so it stays for admission refusals; the rate-limit
  reason goes to the embedder instead.
- Burst defaults to about one second of the cap, as on QUIC.
- Tests: a capped pull stays under cap plus burst; two pulls with different
  caps, each reporting every packet lost for several burst windows, stay
  under their own; a publisher over the cap is closed mid-session, apart from
  a `Forbidden` admission refusal; under the cap nothing changes.

Public API: a rate handle and a rate-limit close reason on moq-srt's server.
Wire: none.

## Required

- [SRT on the shared socket](/quest/m2/one-port/srt-demux.md) - the direct `srt-protocol` integration this hooks into

## Related

- [moq-rtc pacing](/quest/m2/rtc-pacing.md) - the same pacing and retransmit charging on WHEP
- [moq-srt wire source](/quest/m2/wire-stats/srt.md) - per-connection byte counts from the same socket
