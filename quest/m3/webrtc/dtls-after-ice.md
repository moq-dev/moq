# [S] DTLS waits for ICE

## Goal

DTLS sends its first flight only once ICE has nominated a pair, so a peer whose ICE connects late (near the 30 s ICE deadline) still completes the DTLS handshake, and a session that never connects ends with `IceTimeout`.

## Plan

Decided 2026-10-30 with the maintainer (planning interview). Today DTLS starts before ICE connects and dimpl gives up after 1+2+4+8+16 s with jitter (`rs/moq-webrtc/src/vendor/dimpl/config.rs:86-88`), racing moq-webrtc's ICE module's 30 s deadline; [moq-dev/moq#7379](https://github.com/moq-dev/moq/pull/7379) papered over the race by reporting an early DTLS failure as a timeout. Hold DTLS activation (str0m's `Rtc::init_dtls` in `rs/moq-webrtc/src/vendor/str0m/lib.rs`) until nomination, then remove #7379's remapping if nothing else needs it. Test on the paused clock: ICE nominating at 29 s still completes DTLS. Added 2026-10-31 from [moq-dev/moq#7379](https://github.com/moq-dev/moq/pull/7379): also test that a DTLS failure after ICE has connected still reports a DTLS error (moq-webrtc's `ice::Error::Rtc`, or the [typed handshake timeout](/quest/m3/webrtc/dtls-timeout.md)), not `Timeout` (moq-webrtc's `IceTimeout`), so the remapping (or its removal) only ever changes a failure before nomination.

Public API: none. Wire: none.
