# [S] Interop audio tone survives a cold start

## Goal

The interop "Media output and lifecycle" step stops failing the audio tone
check on cold start. The 2026-09-28 nightly on `main` failed with
`FAIL audio tone: cold start: 55/64 samples carried the fixture tone above the
noise floor`, against a 90% agreement floor.

## Plan

- Reproduce it: run the interop media step in a loop on a loaded machine
  until the cold-start row fails, and capture which samples miss (the first
  ones, or scattered).
- Find the cause before touching the check: the likely suspects are decoder
  or playout warm-up after the context resumes. [Publish channel count](/quest/m1/publish-audio-channel-count.md)
  saw the same symptom, but `test/interop/clients/js/src/fixture.ts` already
  dropped the `channelCount` override, so that's not it.
- Fix it at the source. The cold-start window deliberately starts once the
  context runs, without waiting for a tone, so a slow start has to fail
  there. Keep that window: if the misses are the first samples, find what
  delays the first audible output instead of moving the window. Don't lower
  `AGREEMENT` (`test/interop/clients/js/media.ts`) without a measured reason.

Public API: none. Wire: none.

## Related

- [More tests under load](/quest/m1/test-flakes-2.md) - the other load-only failures
