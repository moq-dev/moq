# [M] Cover denied capture permission in the browser

## Goal

The browser harness proves what `<moq-publish>` does when the user refuses the
camera or microphone: no broadcast is announced, the failure is visible through
the public API, and a subscriber gets a clean "nothing is published" rather than
a hang or a half-announced broadcast. Granting permission afterwards recovers
without a reload.

## Plan

`test/smoke/clients/js/media.ts` already runs Chromium without
`--use-fake-ui-for-media-stream` and asserts the user-gesture gate on audio, but
every media case it drives uses the generated fixture, which never calls
`getUserMedia`. The permission path is still only exercised by the interop
matrix, which grants everything up front.

- Drive `<moq-publish source="camera">` in a context where capture is denied.
  `--use-fake-device-for-media-stream` keeps the device deterministic while
  Playwright's permission state decides the verdict, so the case stays
  repeatable.
- Assert `announce="source"` holds the broadcast back, that the element surfaces
  the denial through its public state rather than only a console warning, and
  that a subscriber sees no announcement.
- Grant the permission and assert the broadcast announces and encodes without
  reloading the page.
- Publish what the case cannot claim: a fake device is not a real one, and a
  headless permission decision is not a user clicking a prompt.
- Assert the audio gesture gate while there. `media.ts` clicks both pages and
  requires audio afterwards, but does not assert silence beforehand: Chromium
  enforces the gate on the fixture page and has been seen not enforcing it on the
  player's, whose graph is built a second later. Find what actually decides it,
  then assert the gate rather than only exercising it.

## Related

- [Failure artifacts](/quest/m0/qa-failure-artifacts.md) - shared trace and sample output
