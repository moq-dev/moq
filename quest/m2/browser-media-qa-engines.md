# [M] Run the media QA harness with a Firefox or WebKit player

## Goal

The media output and lifecycle checks measure a player on more than the
pinned Chromium, and an engine that cannot run a case says so by name
instead of passing by omission. A green run states which engines were
measured and which APIs each one lacked.

## Plan

`just test media` measures presented frame progress, tone presence,
and audio/video skew against a deterministic fixture, but `harness.ts`
launches only Playwright's Chromium and nothing installs another engine. Its
capability probe already enumerates what the player needs (WebTransport,
WebCodecs encode and decode, AudioWorklet, `MediaStreamTrackProcessor`,
`canvas.captureStream`) and fails naming what is missing.

Neither Playwright Firefox nor Playwright WebKit ships WebTransport, and
WebKit also lacks the WebCodecs encoder, so neither can publish and neither
can play over QUIC. What they can do is play over the WebSocket fallback.
Real Firefox does use WebTransport; only Playwright's build lacks it, so a
fallback run covers the player, not Firefox's transport path (see
[#4460](https://github.com/moq-dev/moq/pull/4460), merged 2026-10-01).

- Split the harness's publisher and subscriber roles so an engine can take
  one side: a Chromium fixture with a Firefox or WebKit player over the
  fallback is the case worth having.
- Install the engines the run asks for and report a per-engine matrix. An
  engine that cannot publish is not a skipped cell; it is a stated limit with
  the missing API named, and a green run says which engines it measured.
- Keep the tolerances shared. A skew bound that only holds on one engine is
  a measurement of that engine, not of the player.
- Playwright WebKit is not Safari. Say so in the report; a Safari defect such
  as #2812 still needs a manual run.
- Real Firefox 155 still needs a manual run too: it reports a non-empty
  `WebTransport.protocol` and lands on lite-06 against the in-tree relay, while
  153/154 take the draft-14 SETUP path.
