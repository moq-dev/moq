# [S] Nightly media players pass on Firefox and WebKit, or leave the matrix

## Goal

Nightly's `Media (firefox player)` and `Media (webkit player)` jobs pass, or
an engine that cannot play the fixture leaves the matrix with its limit
recorded. Neither has passed since
[#5640](https://github.com/moq-dev/moq/pull/5640) added them on 2026-10-16,
so every nightly is red for a known cause and hides new failures.

## Plan

Found by the 2026-10-19 test cost audit; the same failure each night
(for example [webkit](https://github.com/moq-dev/moq/actions/runs/39266265648/job/118106525118)
and [firefox](https://github.com/moq-dev/moq/actions/runs/39266265648/job/118106525416)):

- WebKit 26.6 and 27.2 fail the capability check: "cannot run player:
  missing decode avc1.42001f, decode vp8, decode opus". Every row then fails.
  Playwright's Linux WebKit is not Safari, so this looks like a limit of the
  build rather than a player bug. #5640 left exactly this call open.
- Firefox 157 fails each row at "gesture starts audio: waiting for the audio
  graph to resume after a user gesture". #5640 saw this locally once and
  could not reproduce it on a host with an audio server; the arm runner may
  have none.

Open decision for WebKit, recommended first:

1. Drop it from the nightly matrix and record the missing decoders, since a
   row that cannot pass measures nothing. Real Safari coverage stays with
   [WebGPU on Safari](/quest/m1/webgpu-safari.md) and manual runs.
2. Keep it and report the capability gap without failing the job, which
   contradicts the harness's fail-by-name rule.

For Firefox, reproduce on an arm runner image first. If the cause is the
missing audio device, give the job one (for example a null PulseAudio sink)
rather than relaxing the gesture check.

Public API: none. Wire: none.
