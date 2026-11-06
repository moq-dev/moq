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

2026-11-06 23:53 UTC rerun after merging current main: dependency installation
passed before overlap, and `quest gate just ci` passed (112 seconds), including
`test gh`. Both overlapping `just test media --media-cases pause` runs exited 1
before browser startup. Runs `interop-iPWHd34k` and `interop-wdFesAVD` failed
inside the harness's own concurrent Bun installs: `Failed to link semver:
EEXIST` (and concurrently, vite, yaml, and @babel/parser). Both reported
`media checks (browser client unavailable)`. Neither created a PulseAudio
server or `pulse.log`; this attempt gives no new daemon-isolation evidence.
Outputs and full run directories are retained in the implementation checkout's
`.tmp/`. Recommend serializing the harness dependency installation while keeping
the two playback runs concurrent, then repeating the overlap unchanged.

Decided 2026-11-06 (maintainer, /quest-complete): ✅ retain this quest until
overlapping media runs pass. Retiring it based on clean daemon logs was not chosen.

## Waiting

- moq-dev/moq: resolve concurrent Bun install link collisions in the media harness, then demonstrate two overlapping `just test media --media-cases pause` runs pass. Preinstalling dependencies alone did not prevent the internal installs from colliding. Retain existing daemon isolation and this quest pending that validation.
