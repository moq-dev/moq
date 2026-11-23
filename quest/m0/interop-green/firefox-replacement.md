# [M] Firefox plays a live replacement and its fallback within the bound

## Goal

`Media (firefox player)` in the scheduled Nightly passes the `replacement`
case on every wire: the viewer shows the new run's picture and tone within
1039 ms of its announcement while the incumbent stays open, and the still-live
older run's within 1039 ms of the newest withdrawing.

## Plan

The case has failed on Firefox every night since it first ran there
(2026-11-19), including after
[moq-dev/moq#9293](https://github.com/moq-dev/moq/pull/9293) (live join) and
[moq-dev/moq#9300](https://github.com/moq-dev/moq/pull/9300) landed:

| Run | Wire | Check |
| --- | --- | --- |
| [44767045952](https://github.com/moq-dev/moq/actions/runs/44767045952) (11-20) | `moq-lite-07-wip` | `live incumbent fallback plays` |
| [44916717292](https://github.com/moq-dev/moq/actions/runs/44916717292) (11-21) | `moq-lite-06` | `live incumbent fallback plays` |
| [45035580806](https://github.com/moq-dev/moq/actions/runs/45035580806) (11-22) | `moq-lite-07-wip` | `live replacement plays` |

Each failing state has `audioStalled: true` and `rms: 0`; on 11-22 the new
run's picture had not painted either, 0.9 s after the relay saw the viewer
subscribe to it (`relay.log` in the run's `media-firefox` artifact).

Every one of those nights, the lifecycle `[player]` page, which stays open and
playing through the `replacement` section, underflowed at the same moment as
the replacement viewer: 1506 ms on 11-20, 102 ms on 11-21, 1759 ms on 11-22.
In the 11-22 trace that page's 50 ms state samples stretched to 310 ms just
before. Nothing links that page to the replacement broadcast, so the runner
(`ubuntu-24.04-arm`, 4 vCPUs, software decode) starved every page at once.
During the section it runs five media pages: the lifecycle fixture and player,
the old and new fixtures (each encoding), and the replacement viewer.

Chromium on the x86 hosted runner passes the section in scheduled Interop
(11-21, 11-22). Locally on `main` at `a5345c7dba`, on a host at load 20 to 28
of 24 CPUs, `just test media --media-cases replacement --media-engines
firefox` passed 6 of 6 (x86, so not the runner's decode cost); full Chromium `just test media` runs failed the section's
audio tone 2 of 4 times with long animation frames in the player, the same
starvation.

Recommended, for review: detach the lifecycle player for the section (its
page's `detach` command, `test/interop/clients/js/src/setup.ts`, then
`reattach` before the cases after it), and give the lifecycle fixture a way to
stop encoding there too. Measure it differentially: the Firefox
media job's failure rate with and without, on the same runner class. If the
case still fails with the runner idle, the cause is in the player (the audio
ring never anchoring after a route switch) and this quest takes it. Loosening
the 1039 ms bound or retrying is not a fix.

Public API: none. Wire: none.
