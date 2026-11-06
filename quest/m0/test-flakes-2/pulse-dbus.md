# [S] Test PulseAudio servers stay off the desktop's D-Bus

## Goal

Every private PulseAudio server the test suites start stays off the session
bus, so two media runs on one desktop never collide on the
`org.PulseAudio1` name. Concurrent `just test media` runs on a desktop host
pass where today one can fail with `D-Bus name org.PulseAudio1 already taken`.

## Plan

Planned 2026-11-05 (maintainer, /quest-plan of the follow-ups from the
2026-11-05 /quest-complete run), from
[moq-dev/moq#7716](https://github.com/moq-dev/moq/pull/7716). Milestone: ✅ m0.

The daemon-level name registration is independent of D-Bus modules
([PulseAudio 17 startup](https://github.com/pulseaudio/pulseaudio/blob/v17.0/src/daemon/main.c#L1202-L1211)).
[moq-dev/moq#7831](https://github.com/moq-dev/moq/pull/7831) already isolated
that launch path before this quest was recreated. All explicit media,
native-play, and native audio-quality launches use `test/lib/pulse.py`.

2026-11-05 verification: `python3 test/lib/pulse.test.py` passes on current
main. A scratch copy restoring the daemon's session-bus inheritance fails
with `D-Bus name org.PulseAudio1 already taken` under the test's private bus.
No desktop services were modified. No remaining launch path has been reproduced.

2026-11-06 validation: main includes the merged
[dev-shell environment fix](https://github.com/moq-dev/moq/pull/7979).
`nix develop --command quest gate bun install --frozen-lockfile` and
`nix develop --command python3 test/lib/pulse.test.py` pass.

The previous `--media-cases none` command now selects an empty focused case
set, which the browser runner rejects. Validate two overlapping
`just test media --media-cases pause` runs instead, without changing the
harness environment. Dependency installation is complete before overlap.

The overlapping pause runs both failed after reaching playback (2026-11-06
06:53 UTC): run `interop-JSI7CQaU` failed `resume takes effect` with the
presented frame stuck at 31; run `interop-om6rAC4x` failed `audio tone` after
resume (63/75 samples). The latter recorded host idle time of 0.1–2.9%,
which suggests contention but does not establish the cause.

Both runs' `pulse-1/pulse.log` show `Daemon startup complete.`, independent
runtime directories and normal SIGTERM teardown. Neither reports a D-Bus
name collision. Both warn that D-Bus autolaunch is unavailable without
`DISPLAY`, then successfully start. No failing daemon launch was found.
The relay environment rejection and Bun install collisions did not recur.

These runs do not meet the requested passing-overlap condition for retirement.
Keep the existing isolation. Investigate the resume failures separately before
claiming an overlap pass; do not infer a PulseAudio defect from these failures.

Public API: none. Wire: none.

## Waiting

- Maintainer: choose whether to retain this quest pending a passing overlap or retire it based on the existing daemon-isolation test and successful daemon startups. Recommend retaining it pending validation of the resume failures; the requested passing-overlap condition was not met. An agent cannot authorize deletion.
