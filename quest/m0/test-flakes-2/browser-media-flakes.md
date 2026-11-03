# [S] Firefox audio-tone tests pass under load

## Goal

The Firefox player's `audio tone: after resume` and `audio tone: coded-size
growth` checks pass reliably in Interop and Nightly, fixed at their demonstrated
cause under the epic's rules. Do not increase deadlines or weaken playback or
tone assertions.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31
/quest-complete run), from [moq-dev/moq#7402](https://github.com/moq-dev/moq/pull/7402)
and [moq-dev/moq#7255](https://github.com/moq-dev/moq/pull/7255).

[moq-dev/moq#7716](https://github.com/moq-dev/moq/pull/7716) handles the other
two original goals:

- The buffering fixture omitted the video configuration signal introduced by
  [moq-dev/moq#7155](https://github.com/moq-dev/moq/pull/7155). The effect threw
  before subscribing to Pause, leaving the spinner visible forever. Both
  failures reproduce on current main; supplying the fixture signal fixes them.
  This correction is identical to the fixture change already proposed in
  [moq-dev/moq#7672](https://github.com/moq-dev/moq/pull/7672); no workflow
  deadline change is included here.
- The Nix shell supplies libavcodec and libpulse only to Playwright Firefox's
  launch environment. Without them, the local player fails its H.264 capability
  probe before playback.

The tone failures remain unconfirmed on current source. The original report
measured main's fixture at 2/3 for `rename,pause`, failing during coded-size
growth. Re-read `test/interop/clients/js/media.ts` and the current scheduled
evidence before changing measurement: `measurePlayback` already waits for a
running context and a filled audio ring, without waiting for a tone. The
[2026-11-02 Nightly](https://github.com/moq-dev/moq/actions/runs/41483654426/job/125408204778)
passed resize and resume tone checks; it failed a separate incumbent fallback.
A passing run does not establish the cause of the reported tone failure.

The JS pause/resume cause in
[moq-dev/moq#7709](https://github.com/moq-dev/moq/pull/7709) is separate from
this measurement goal. Its passing IETF cells do not establish this goal.

Public API: none. Wire: none.

## Related

- [Interop and Video pass on main](/quest/m0/interop-green/README.md) - the scheduled Interop run these tests fail in
