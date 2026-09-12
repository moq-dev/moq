# [S] Unlock the publisher's capture AudioContext on a gesture

## Goal

A page that starts publishing before the user has interacted with it still captures audio, or says
why it cannot. `<moq-publish>` never announces an audio track that silently carries nothing.

## Plan

`@moq/publish`'s `Audio.Capture` builds its own `AudioContext` the moment a track source appears
(`#runTrack`, `js/publish/src/audio/capture.ts:130`, the context at `:143`) and never resumes it.
`@moq/watch`'s decoder handles the same problem with `unlockOnGesture`; the publisher has no
equivalent.

Observed in `test/smoke/clients/js`, which drives Chromium with no autoplay override: handing the
capture its source at page load left the audio rendition without a catalog config indefinitely, and
the fixture only became ready once the source was withheld until after a real click. The fixture
carries that workaround today (`test/smoke/clients/js/src/fixture.ts:109`), with a comment pointing
here.

- Confirm the mechanism before fixing it. The suspended capture context is the likely cause (a
  suspended context renders nothing, so the capture worklet never posts the first frame that sets
  the captured format), but the observation above is a symptom, not a proof: reproduce it in
  `capture.test.ts` first.
- Then either apply what `unlockOnGesture` gives the decoder, or refuse the source with a clear
  error rather than announcing a track that will never carry samples.
- The permission-granted path can reach `#runTrack` with no gesture at all (a pre-granted camera on
  page load), so a gesture is not something the capture can assume already happened.
- Drop the workaround in `test/smoke/clients/js/src/fixture.ts` and let the harness assert the
  behavior instead.

## Related

- [Browser permission QA](/quest/m2/browser-permission-qa.md) - the other publisher path that runs without a gesture
