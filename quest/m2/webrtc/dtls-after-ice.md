# [S] DTLS waits for ICE

## Goal

DTLS sends its first flight only once ICE has nominated a pair, so a peer whose ICE connects late (near the 30 s ICE deadline) still completes the DTLS handshake, and a session that never connects ends with `IceTimeout`.

## Plan

Decided 2026-10-30 with the maintainer (planning interview). Today DTLS starts before ICE connects and dimpl gives up after 1+2+4+8+16 s with jitter (`dimpl/src/config.rs:90-92`), racing moq-ice's 30 s deadline; [moq-dev/moq#7379](https://github.com/moq-dev/moq/pull/7379) papered over the race by reporting an early DTLS failure as a timeout. Hold DTLS activation (around str0m's `lib.rs:1493`) until nomination, then remove #7379's remapping if nothing else needs it. Test on the paused clock: ICE nominating at 29 s still completes DTLS.

Public API: none. Wire: none.

## Required

- [Vendor and trim the stack](/quest/m2/webrtc/vendor.md) - the DTLS activation this moves
