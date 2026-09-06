# [L] Verify browser media output and lifecycle

## Goal

The browser harness proves that viewers receive advancing video and meaningful
audio with bounded synchronization error, and recover through supported
publication lifecycle changes. Transport bytes or a running AudioContext alone
do not satisfy the media-output assertion.

## Plan

`test/smoke/clients/js/driver.ts` already checks painted video, pause/resume,
UI controls, and audio bytes with a running AudioContext. Extend that harness
instead of building a separate demo-only test framework.

- Generate deterministic changing video with frame IDs and timed audio markers.
  Measure presented frame progress, decoded audio samples, and marker alignment
  at the playback sink. Specify tolerances from the fixture and playback policy,
  and report browser output measurements separately from physical speaker output.
- Cover late join, publisher stop and same-path republish, unsubscribe/rejoin,
  and element detach/reattach through public APIs. Verify pending operations
  settle and sessions, tracks, workers, and audio resources return to baseline.
- Preserve the current pause/resume coverage. Make publisher readiness explicit
  and run a cold-start case without the driver's halfway page reload, so reload
  cannot conceal a missed announcement or initialization bug.
- Add focused permission-denied and user-gesture/autoplay cases without the
  permissive launch flags. Keep fake media for repeatability and publish the
  limits of those cases beside any real-device verdict.
- Start with the pinned Chromium target. Inventory supported browser capabilities
  before adding other engines; report unsupported APIs explicitly instead of
  treating a skipped media case as cross-browser success.

Acceptance: silence the audio sink, freeze video after its first frame, offset
one track, and prevent old-session teardown in controlled fixtures. Each
corresponding assertion must fail. Run the normal cases with a real local relay
and retain timing samples and traces for diagnosis.

## Related

- [Failure artifacts](/quest/m0/qa-failure-artifacts.md) - shared trace and sample output
- [Republished broadcast](/quest/m0/3363-js-watch-a-broadcast-republished-on-one-session-keeps-resuming.md) - owns the existing resume defect and its focused regression
- [Open-GOP tune-in](/quest/m2/2067-test-open-gop-h-264-tune-in-end-to-end-leading-picture.md) - owns codec-specific recovery fixtures
