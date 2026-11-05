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

Two overlapping `just test media --media-cases none` runs in the Nix shell
both stop before browser startup: the relay rejects `MOQ_QUEST`, which the
shell exports. Concurrent Bun installs also collide in the shared
`node_modules` (`EEXIST`/`ENOENT`). Do not infer an audio result from these runs.

After the shell fix lands, finish dependency installation before starting
the overlapping runs. If those pass, retire this duplicate quest; otherwise
trace the failing daemon from its run's `pulse.log` and isolate that source.

Public API: none. Wire: none.

## Waiting

- moq-dev/moq: land [the dev-shell environment fix](https://github.com/moq-dev/moq/pull/7979), then rerun overlapping media cases without altering the harness environment. Recommendation: retain the existing isolation unless a remaining audio collision is reproduced.
