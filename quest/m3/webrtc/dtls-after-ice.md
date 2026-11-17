# [S] DTLS waits for ICE

## Goal

DTLS sends its first flight only once ICE has nominated a pair, so a peer whose ICE connects late (near the 30 s ICE deadline) still completes the DTLS handshake, and a session that never connects ends with `IceTimeout`.

## Plan

Decided 2026-10-30 with the maintainer (planning interview). Today DTLS starts before ICE connects and dimpl gives up after 1+2+4+8+16 s with jitter (dimpl 0.9's `flight_start_rto` of 1 s and `handshake_timeout` of 40 s, `src/config.rs:92-94`, which rind-str0m's DTLS provider leaves at their defaults), racing moq-webrtc's ICE module's 30 s deadline; [moq-dev/moq#7379](https://github.com/moq-dev/moq/pull/7379) papered over the race by reporting an early DTLS failure as a timeout. Hold DTLS activation (str0m's `Rtc::init_dtls`) until nomination, then remove #7379's remapping if nothing else needs it. Test on the paused clock: ICE nominating at 29 s still completes DTLS. Added 2026-10-31 from [moq-dev/moq#7379](https://github.com/moq-dev/moq/pull/7379): also test that a DTLS failure after ICE has connected still reports a DTLS error (moq-webrtc's `ice::Error::Rtc`, or the [typed handshake timeout](/quest/m3/webrtc/dtls-timeout.md)), not `Timeout` (moq-webrtc's `IceTimeout`), so the remapping (or its removal) only ever changes a failure before nomination.

Changed 2026-11-16 (maintainer, /quest-plan of the pre-cut audit): moq-webrtc [returns to upstream str0m](https://github.com/moq-dev/moq/pull/8963), so `Rtc::init_dtls` is no longer ours to move. In 0.25 it is private (`src/lib.rs:1519`), called from `accept_offer` and `accept_answer` through `init_dtls` (`src/change/sdp.rs:727`). Start by reading how str0m 0.25 starts DTLS and whether its public API can hold the first flight until ICE nominates. If it can, do it there and keep the tests above. If it cannot, the fix is a change inside str0m: write the report for [upstream reports](/quest/m4/upstream-reports.md), which the maintainer approves before it is filed, keep #7379's remapping, and reduce this quest to waiting on upstream.

Public API: none. Wire: none.
