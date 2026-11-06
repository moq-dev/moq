# [S] Push-to-talk's acceptance proof covers native playback

## Goal

`just test push-to-talk` (`test/audio-quality/ptt.sh`) runs a native
`moq play --metrics` listener beside the browser one, on its own session
through the same local relay, and holds it to the epic's correctness checks:
every utterance is heard from its first frame, each muted span's declared
silence plays as comfort noise, and no press causes an underrun or stall.

Native playback learned to fill declared silence with comfort noise in
[moq-dev/moq#8010](https://github.com/moq-dev/moq/pull/8010), but only unit
tests cover it. The audio-quality harness's `pulse` rows grade native playback,
never through a mute and unmute.

Non-goal: a native latency budget.
[Press-to-audible latency](/quest/m1/push-to-talk-latency.md) stays
browser-only, as its Goal records.

## Plan

Decided 2026-11-06 (maintainer, /quest-complete while queueing
[moq-dev/moq#8010](https://github.com/moq-dev/moq/pull/8010)): ✅ plan it
as a child of [Push-to-talk](/quest/m1/push-to-talk/README.md), checking
correctness only. Not chosen: folding it into the latency quest and budgeting
native latency; dropping it because unit tests and the `pulse` rows suffice.

`moq play --metrics` already reports underruns and needs `--video off`. Run
it under the same ideal-path conditions as the browser listener (no shaping
or loss), and report its rows in the same `summary.json` so one press's
browser and native results sit together. If the native metrics cannot tell
a lost utterance head from comfort noise, add what it needs to `--metrics`
rather than inferring it in the script.
