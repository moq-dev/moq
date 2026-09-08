# [S] Unlock the publisher's capture AudioContext on a gesture

## Goal

A page that starts publishing before the user has interacted with it still captures audio, or says
why it cannot. `<moq-publish>` never announces an audio track that silently carries nothing.

## Plan

`@moq/publish`'s `Audio.Encoder` builds its own `AudioContext` the moment a source appears
(`#runSource`) and never resumes it. `@moq/watch`'s decoder handles the same problem with
`unlockOnGesture`; the publisher has no equivalent.

Observed in `test/smoke/clients/js`, which drives Chromium with no autoplay override: handing the
encoder its source at page load left the audio rendition without a catalog config indefinitely, and
the fixture only became ready once the source was withheld until after a real click. The fixture
carries that workaround today, with a comment pointing here.

- Confirm the mechanism before fixing it. The suspended capture context is the likely cause (a
  suspended context renders nothing, so the capture worklet never posts the first frame that sets
  the captured format), but the observation above is a symptom, not a proof: reproduce it in a
  `js/publish` test first.
- Then either apply what `unlockOnGesture` gives the decoder, or refuse the source with a clear
  error rather than announcing a track that will never carry samples.
- The permission-granted path can reach `#runSource` with no gesture at all (a pre-granted camera on
  page load), so a gesture is not something the encoder can assume already happened.
- Drop the workaround in `test/smoke/clients/js/src/fixture.ts` and let the harness assert the
  behavior instead.

## Related

- [Browser permission QA](/quest/m0/browser-permission-qa.md) - the other publisher path that runs without a gesture
