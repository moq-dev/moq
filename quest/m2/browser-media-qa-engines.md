# [M] Run the media QA harness with a Firefox or WebKit player

## Goal

The media output and lifecycle checks measure a player on more than the
pinned Chromium, and an engine that cannot run a case says so by name
instead of passing by omission. A green run states which engines were
measured and which APIs each one lacked.

## Plan

`just test media` measures presented frame progress, tone presence,
and audio/video skew against a deterministic fixture, but `harness.ts`
supports pinned Chromium, Firefox, and WebKit players with a Chromium fixture.
Its capability probe enumerates WebTransport, WebSocket, WebCodecs encode and
decode, AudioWorklet, `MediaStreamTrackProcessor`, and `canvas.captureStream`,
then names what is missing separately for publisher and player roles.

Playwright capabilities change with the pinned engine. A native probe of
Playwright 1.63 Firefox 155 found WebTransport and WebCodecs, but no
`MediaStreamTrackProcessor`, so it can play but cannot publish this fixture.
The alternate players explicitly use WebSocket to isolate player behavior;
this does not prove their QUIC transport. Probe publisher and player roles
separately instead of assuming API support from the engine name.

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

## Validation evidence (2026-10-15)

The engine role split, shared graders, explicit WebSocket alternate players,
read-only pre-gesture observations, and nightly Chromium/Firefox/WebKit matrix
are implemented. This quest remains open until a complete alternate-engine
media run passes.

Firefox 155.0 renders the fixture over WebSocket and passes the pre-gesture
gate, but its audio context remains suspended after the real click with
`userActivated: true`; `gesture starts audio` correctly fails. A standalone
AudioContext also remained suspended across pointerdown, pointerup, and click
resume attempts. The production cause is not established. Negative controls
that fail at this earlier gate are failures, not sensitivity proof.

WebKit 2359 is installed locally but cannot launch because host runtime
libraries are absent. This is a runtime dependency failure, not evidence about
WebKit APIs. Nightly installs dependencies with Playwright `--with-deps`.
No tolerance or autoplay gate has been relaxed.

The default Chromium path still enforces its original gate and tolerances. Its
full run fails cold-start tone presence at 65/80 samples against the unchanged
90% floor, matching the previously recorded class of cold-start refusal in
#5635. This does not establish a common cause with the Firefox startup failure.
The Chromium silent-audio, offset-audio, leaked-session, and lagging-latecomer
negative controls pass. Its frozen-video control instead fails publisher
readiness with a suspended fixture context, so that row is not sensitivity
proof in this run. No repeat was used to turn either failure into a pass.
