# [S] The publish element's mute and hide keep capture open

## Goal

`<moq-publish muted>` and `<moq-publish invisible>` stop sending audio or
video without closing the microphone or camera. Unmuting pays no
getUserMedia, no new AudioContext and no worklet load, and unhiding pays no
camera reopen. Removing `source`, or removing the element from the
document, still releases both devices.

## Plan

Traced 2026-10-28 from [#7101](https://github.com/moq-dev/moq/issues/7101):
`muted` sets `audioEnabled = enabled && !muted` (`js/publish/src/element.ts:204`),
and that one signal drives the `Source.Microphone`, the Capture and the
encoder (`:430-431`). Turning it off tears down the AudioContext and worklet
(`capture.ts:177`) and stops the track (`microphone.ts:61`). Unmuting then
pays 50-300+ ms on real hardware before any frame exists. `invisible` does
the same to the camera. Toggling `Audio.Encoder.enabled` alone leaves
capture running.

Decided 2026-10-28 (maintainer):

- **`muted` keeps the mic open.** It disables only the audio encoder; the
  source and Capture follow the element's lifecycle gate (`#enabled`, cleared
  by `disconnectedCallback`, `element.ts:356-359`) and `source`, independent
  of `muted` and `invisible`. No new attribute: a second mute flag would be
  the same control twice.
- **`invisible` does the same for video.** It disables only the video
  encoder and leaves the camera open. One rule for both, and removing
  `source` is how a page releases the devices. Rejected: keeping `invisible`
  releasing the camera as a privacy cue.
- The browser's microphone and camera indicators stay lit while muted or
  hidden. The docs say so, and say that releasing one device while keeping
  the other takes the library (`Source.Microphone` or `Source.Camera`
  `enabled`), since `source` covers both.

Docs: the attribute table in `doc/lib/js/publish.md:34` ("Disable audio or
video capture") and the element's doc comments.

Tests: muting keeps the microphone track live and unmuting encodes without a
new getUserMedia. Hiding keeps the camera track live. Removing `source`
stops both tracks, and so does detaching the element; reattaching it
reopens them.

Public API: behavior change to `muted` and `invisible` (capture stays open).
Wire: none.
