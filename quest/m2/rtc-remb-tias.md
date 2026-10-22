# [M] moq-rtc steers WHIP publishers to a bitrate cap

## Goal

An embedder can give a `moq-rtc` WHIP session a bitrate cap, and moq-rtc
steers the browser's encoder under it: a `b=TIAS` and `b=AS` line in the SDP
answer, and REMB where the publisher negotiates it. Media past the cap plus a
burst anyway (a client ignoring both) closes the session with a typed error.
This backs moq.pro's ingest caps.

## Plan

Decided 2026-10-20 (maintainer): planned upstream now, because moq.pro's
WHIP enforcement needs it and nobody owned it.

- moq-rtc has no REMB or bandwidth-line support today. Add both on the WHIP
  server: TIAS and AS from the cap in the answer, and periodic REMB at or
  below the cap. TWCC carries arrival times, not an estimate, so there is
  nothing to clamp there.
- A per-session rate handle sets the cap after signaling and accepts live
  updates; REMB follows an update at once. Burst defaults to about one
  second of the cap, as on QUIC.
- Meter received media against the cap plus burst and close past it with a
  typed rate-limit error the embedder can log.
- Tests: an answer carries TIAS and AS for a capped session and neither for
  an uncapped one; REMB never exceeds the cap; a publisher sending over the
  cap plus burst is closed; under the cap nothing changes.

Public API: a rate handle and a rate-limit error on moq-rtc's WHIP server.
Wire: SDP bandwidth lines and RTCP REMB.

## Related

- [moq-rtc pacing](/quest/m2/rtc-pacing.md) - the WHEP half of the same caps
- [SRT rate handle](/quest/m2/srt-rate-handle.md) - the same caps on SRT
