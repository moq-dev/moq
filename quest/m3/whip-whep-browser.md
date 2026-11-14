# [S] A browser round-trips media through moq-webrtc

## Goal

A `test/` recipe publishes from Chromium and Firefox over WHIP to a relay with
moq-webrtc and plays the broadcast back over WHEP, with fake media, and
asserts from `getStats()` that DTLS and SRTP connected and the viewer decoded
frames. It guards the vendored WebRTC stack's crypto and negotiation against
real browsers.

## Plan

Planned 2026-11-08 (maintainer, approved follow-up of
[moq-dev/moq#8229](https://github.com/moq-dev/moq/pull/8229), which moved the
stack onto rind assuming browser interop tests existed). None drives a
browser against moq-webrtc today: no recipe in `test/` or workflow mentions
WHIP or WHEP.

Moved to m3 2026-11-12 (maintainer, /quest-audit): a downstream deployment serves WHIP and WHEP from this stack.

- Reuse the Playwright setup the interop and audio-quality harnesses use
  (`test/interop/clients/js/`), with the browsers' fake capture devices.
- Run it where the other browser tests run; it is slow, so nightly, not the
  merge gate.

Public API: none. Wire: none.
