# [M] moq-publish surfaces a capture denial and recovers when permission is granted

## Goal

When the user refuses the camera or microphone, `<moq-publish>` says so
through its public state, announces no broadcast, and a subscriber sees a
clean "nothing is published" rather than a hang or a half-announced
broadcast. Granting permission afterwards recovers without a reload. The
browser harness proves all of it.

## Plan

Today a denial is invisible. `js/publish/src/source/camera.ts` swallows the
`getUserMedia` rejection (`.catch(() => undefined)`) and hands it to the
retry loop; `microphone.ts` does the same, `device.ts` only warns. `Camera.out`
exposes `source` alone and `Retry` has no public failure signal, so the only
external symptom is a `source` that never becomes defined.

- Give the capture sources a public failure state: the denial (and any other
  terminal `getUserMedia` error) reaches `<moq-publish>` as an observable
  status a page can render, and the element stops retrying a permission the
  user refused. Refused is refused; a silent retry loop is warn-then-ignore.
- Keep `announce="source"` holding the broadcast back while there is no
  source, so a subscriber sees no announcement.
- Regression in `test/smoke/clients/js/media.ts`: drive
  `<moq-publish source="camera">` with `--use-fake-device-for-media-stream`
  so the device is deterministic while Playwright's permission state decides
  the verdict. Assert the denial is visible through the element's state, no
  announcement reaches the subscriber, then grant the permission and assert
  the broadcast announces and encodes without reloading the page. State what
  the case cannot claim: a fake device is not a real one and a headless
  permission decision is not a user clicking a prompt.
- Assert the audio gesture gate while there. `media.ts` clicks both pages
  and requires audio afterwards but does not assert silence beforehand;
  Chromium enforces the gate on the fixture page and has been seen not
  enforcing it on the player's, whose graph is built a second later. Find
  what decides it, then assert the gate rather than only exercising it.

## Related

- [Publisher audio unlock](/quest/m2/publish-audio-unlock.md) - the other publisher path that fails silently without a gesture
- [Failure artifacts](/quest/m2/qa-failure-artifacts.md) - shared trace and sample output
