# [M] Capture Control: settled name, loud cut, prompt cancel, catalog clock

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
- Capture publishers stamp on the clock their catalog advertises, with no
  separate clock to pass. Today both `CaptureOptions` carry their own
  `clock: moq_mux::Clock`, and `Default` builds a fresh one, so a caller
  relying on the default publishes against a mapping the catalog never
  advertised. `moq import capture` passes `catalog.clock()`, the only correct
  value.

## Plan

Decided:

- Rename to `encode::Capture`, which reads as the capture half next to
  `encode::Options`. Update moq-cli and the docs; moq-ffi and moq-c do not
  call the capture paths, so no binding mirror exists today.
- `cut()` fails loud with an error rather than logging. The encoder is opened
  lazily, so the handle may not know yet; the probe already opens one, which
  is one place to learn it early. Choose between `cut()` returning
  `Result` (refusing once the backend is known) and the driver ending with
  `CutUnsupported`, and record the choice here.
- Race every await in the driver against the controls closing, rather than
  only the idle wait, so the probe and the demand-driven opens both cancel.
- Drop the `clock` field from both options; `Control::new` already takes the
  catalog producer, so it reads `catalog.clock()`. Update moq-cli and any
  binding that forwards a clock. The clock fixtures in both crates already
  pass the catalog's clock, so they keep grading the same path. This absorbs
  the former m2 capture-clock-source quest: it breaks the same `dev` options,
  so one break lands instead of two.

This is a `dev` break layered on #4184; land it on `dev` before the release
that first publishes these handles.

Tests: a backend without forced keyframes surfaces `CutUnsupported` to the
caller; dropping the last `Control` during a slow fake open or probe returns
from `Driver::run` without finishing the open.
