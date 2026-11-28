# [M] The media lane's volume check finds audio in every sample

## Goal

The `mute and volume` section of the media output and lifecycle lane
(`checkVolume` in `test/interop/clients/js/media.ts`) finds audible audio at the
graph root in every sample of each volume step, alone and under load, fixed at the
cause rather than by lowering its 90% agreement.

Seen once: scheduled Interop
[45873739689](https://github.com/moq-dev/moq/actions/runs/45873739689) (2026-11-26,
at `b50932876b`), Chromium, `FAIL volume scales the output: volume 0.25: only 27/31
samples carried audio at the graph root`. Volume 1 had just passed at 1.000, so about
200 ms of the 1.5 s window lost audio, not its scaling. Just before the failing step,
the latecomer page sent a `subscribe update` for both its video and `audio/2`
subscriptions, and the rendition switch back before it logged a 200 ms longest audio
gap. No other scheduled run from 2026-11-22 to 2026-11-27 repeats it, nor do the
last 12 failing Nightly runs, and [moq-dev/moq#9809](https://github.com/moq-dev/moq/pull/9809)
measured the whole media lane at 64 of 64 on `main`.

## Plan

Not reproduced yet. The failure artifact of that run (`interop-failure`, kept 7
days) holds `media-chromium--media-output---lifecycle.log` and the latecomer's
Playwright trace. Start from the trace: whether the gap follows the latecomer's
subscribe update (a retune that restarts or skips audio) or a page stall. Then
reproduce with `just test media --media-cases mute-volume` under load and fix that
path in a failing test.
