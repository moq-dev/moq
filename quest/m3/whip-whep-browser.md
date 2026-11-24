# [S] A browser round-trips media through moq-webrtc

## Goal

A `test/` recipe publishes from Chromium and Firefox over WHIP to a relay with
moq-webrtc and plays the broadcast back over WHEP, with fake media, and
asserts from `getStats()` that DTLS and SRTP connected and the viewer decoded
frames. It guards moq-webrtc's crypto and negotiation, str0m on rind, against
real browsers.

## Plan

Planned 2026-11-08 (maintainer, approved follow-up of
[moq-dev/moq#8229](https://github.com/moq-dev/moq/pull/8229), which moved the
stack onto rind assuming browser interop tests existed). None drives a
browser against moq-webrtc today: no recipe in `test/` or workflow mentions
WHIP or WHEP.

Moved to m3 2026-11-12 (maintainer, /quest-audit): a downstream deployment serves WHIP and WHEP from this stack.

Kept in m3 2026-11-16 (maintainer, /quest-plan of the pre-cut audit): the
audit recommended making this recipe a gate for the cut while the stack
stayed vendored; the maintainer chose to
[return to upstream str0m](https://github.com/moq-dev/moq/pull/8963) instead (rejected:
keep the vendored stack and gate the cut on this). That switch is checked by
one manual Chrome publish and play, so this recipe is still the first
automated browser run against the gateway.

- Reuse the Playwright setup the interop and audio-quality harnesses use
  (`test/interop/clients/js/`), with the browsers' fake capture devices.
- Run it where the other browser tests run; it is slow, so nightly, not the
  merge gate.

Public API: none. Wire: none.
