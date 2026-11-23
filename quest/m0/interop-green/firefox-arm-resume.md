# [M] A Firefox player on ARM keeps running through every resume

## Goal

On the Nightly's runner class (`ubuntu-24.04-arm`), `just test media
--media-engines firefox` passes `rejoin reaches live` and `reattach reaches
live`, including its audio/video sync, because the player page's main thread no
longer stops on a resume (pause, rejoin, or reattach) and so skips no audio
groups as missing there.

## Required

- [Reattach old groups](/quest/m0/interop-green/reattach-old-groups.md) -
  [moq-dev/moq#9492](https://github.com/moq-dev/moq/pull/9492) deletes it and
  adds the `waitForLive` failure diagnostics this needs: both pages' long
  animation frames with their longest scripts, the host's busiest processes,
  and how long ago the player's latest sample arrived.

## Plan

Decided 2026-11-23 (maintainer, /quest-run): plan this quest, reproducing on
ARM with #9492's diagnostics first, then finding the cause (relay or player)
before fixing.

Found while validating [moq-dev/moq#9494](https://github.com/moq-dev/moq/pull/9494):
two full Firefox runs on `ubuntu-24.04-arm` (jobs
[fix-full 1](https://github.com/moq-dev/moq/actions/runs/45269012363/job/138023508756)
and its sibling in [run 45269012363](https://github.com/moq-dev/moq/actions/runs/45269012363),
at `42318ce123`, on `moq-lite-06`) failed before the `replacement` section, in
code #9494 does not change:

- `FAIL rejoin reaches live: held a frame 2033ms behind the fixture 2000ms
  after the first frame ...; audio running and buffering with nothing anchored`
- `FAIL resume audio/video sync: reattach reaches live: offset 800ms`

What the artifacts show:

- Neither is a relay drop. The rejoin run's only `group error err=old`
  (21:28:58.82, `media-9880.hang`) is the `control: leaked session` case's
  broadcast, after the lifecycle case's connections closed at 21:28:26. The
  lifecycle window (`media-8089.hang`) has one `fetch abandoned track=video`
  and nothing else. #9494's body attributes the rejoin failure to that `err=old`;
  it does not apply.
- Both are the player page stopping, the signature #9492 found in Chromium's
  scheduled Interop failure of 2026-11-22. In the Playwright trace,
  consecutive `__moqInteropSample`s (`seq` n to n+1) jump on the page clock at
  each resume, and the pause and rejoin jumps are followed at once by
  `skipping missing groups` on audio covering more than a second:

  | Run | Pause resume | Rejoin | Reattach |
  | --- | --- | --- | --- |
  | rejoin failed | 392 ms, skips 2.74 s | 910 + 710 ms, skips 1.16 s | not reached |
  | reattach failed | 648 ms, skips 3.20 s | 807 ms, skips 1.56 s | 775 ms |

  So every resume stops the page; a check fails when the stop outlasts its
  catch-up window.
- This runs after [moq-dev/moq#9377](https://github.com/moq-dev/moq/pull/9377),
  which #9492 named as the likeliest fix for the Chromium stop (a lite-05/06
  copy's per-key fill walk). Either #9377 did not remove the whole stop, or
  this is another task on the same resume path that the slower ARM CPU
  exposes.

Next: reproduce on `ubuntu-24.04-arm` with a temporary `pull_request` workflow
running `just test media --media-engines firefox --media-cases
pause,rejoin,detach` several times per job, as #9494's did, once #9492's
diagnostics have landed. Its long animation frames name the script that holds
the page. Candidates are work proportional to the span of audio keys (µs
timestamps) rather than to the groups held, such as the gap handling in
`js/hang/src/container/consumer.ts` or the lite-06 fill in
`js/net/src/lite/fills.ts`. Confirm it on x86 under CPU throttling, then fix
it at the root and measure the stop on ARM before and after. If no stop
recurs, the relay becomes the suspect again: rerun with the relay at
`moq_net=trace`. Loosening the bounds or retrying is not a fix.

Public API: none expected. Wire: none expected.

## Related

- [Firefox replacement](/quest/m0/interop-green/firefox-replacement.md) -
  [moq-dev/moq#9494](https://github.com/moq-dev/moq/pull/9494) rewrites it to
  the `moq-lite-06` fallback's audio on ARM, which also ends unanchored after
  `skipping missing groups: track=audio`. The same page stop may explain it.
