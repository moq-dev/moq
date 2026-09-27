# [S] Capture Control: settled name, loud cut, prompt cancel

## Goal

The capture handles from [#4184](https://github.com/moq-dev/moq/pull/4184),
on `dev` only, get their final shape before release:

- `encode::CaptureOptions` is `encode::Capture` in both moq-audio and
  moq-video.
- `Control::cut()` on video tells the caller when the backend cannot force a
  keyframe. Today the driver logs one warning on `CutUnsupported` and keeps
  the GOP cadence, so a recording or resume boundary silently never appears.
- Dropping the last `Control` ends the driver promptly, including while the
  startup probe, `capture::open`, `Sink::open`, or an encode is in flight.
  Today those awaits never see the handle close, so a camera or permission
  prompt can outlive its owner.

## Plan

Decided:

- Rename to `encode::Capture`, which reads as the capture half next to
  `encode::Options`. Update moq-cli and the docs; moq-ffi and libmoq do not
  call the capture paths, so no binding mirror exists today.
- `cut()` fails loud with an error rather than logging. The encoder is opened
  lazily, so the handle may not know yet; the probe already opens one, which
  is one place to learn it early. Choose between `cut()` returning
  `Result` (refusing once the backend is known) and the driver ending with
  `CutUnsupported`, and record the choice here.
- Race every await in the driver against the controls closing, rather than
  only the idle wait, so the probe and the demand-driven opens both cancel.

This is a `dev` break layered on #4184; land it on `dev` before the release
that first publishes these handles.

Tests: a backend without forced keyframes surfaces `CutUnsupported` to the
caller; dropping the last `Control` during a slow fake open or probe returns
from `Driver::run` without finishing the open.

## Related

- [Video keyframe flag](/quest/m1/video-keyframe-flag.md) - the same cut throttle, counting cadence keyframes
- [Capture clock source](/quest/m2/capture-clock-source.md) - drops the `clock` field from these options
