# [M] Run the media QA harness on other browser engines

## Goal

The media output and lifecycle checks run on more than the pinned Chromium, and
an engine that cannot run a case says so by name instead of passing by omission.
A green run states which engines were measured and which APIs each one lacked.

## Plan

`just test smoke-media` measures presented frame progress, tone presence, and
audio/video skew against a deterministic fixture, but only on Playwright's
Chromium. Its capability probe already enumerates what the player needs
(WebTransport, WebCodecs encode and decode, AudioWorklet,
`MediaStreamTrackProcessor`, `canvas.captureStream`) and fails naming what is
missing, which is the groundwork this builds on.

- Take the inventory first. Firefox has no WebTransport in Playwright and WebKit
  has neither WebTransport nor the WebCodecs encoder, so decide per engine
  whether it subscribes only, runs over the WebSocket fallback, or is out of
  scope, and record why.
- Split the harness's publisher and subscriber roles so an engine can take one
  side: a Chromium fixture with a Firefox player is the case worth having.
- Report a per-engine matrix. An engine that cannot publish is not a skipped
  cell; it is a stated limit with the missing API named.
- Keep the tolerances shared. A skew bound that only holds on one engine is a
  measurement of that engine, not of the player.

## Related

- [Failure artifacts](/quest/m0/qa-failure-artifacts.md) - shared trace and sample output
