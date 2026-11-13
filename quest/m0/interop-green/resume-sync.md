# [S] Resumed audio snaps to the clock

## Goal

A viewer that pauses and resumes hears its audio in sync with the picture from the first frame, as it did before [moq-dev/moq#8507](https://github.com/moq-dev/moq/pull/8507). So `just test media`'s pause and rejoin steps pass on every run, and a resumed viewer is not seconds behind for the stretch to catch up. Audio that falls behind because of network delay still catches up by playing faster; that is #8507's stretch and stays.

Not in scope: the JS max-age expectation ([#8572](https://github.com/moq-dev/moq/pull/8572)) and the `go -> python` media cell.

## Plan

Found by the triage of scheduled Interop run [43260674067](https://github.com/moq-dev/moq/actions/runs/43260674067) (2026-11-12, head e9c6df852). Its `Media output and lifecycle` step failed on the pause case: `audio tone: after resume: 70/81 samples` in CI, and locally `audio/video sync: after resume: 0/79 samples within 200ms, median skew 600ms`. The 11-11 run, before #8507, passed that step.

Cause: #8507 keeps audio that trails the shared clock by up to the stretch's 2 s bound and plays it faster to catch up (`js/watch/src/audio/ring-buffer.ts` `read`, bound from `js/watch/src/audio/latency.ts` `bound`). Before it, the ring skipped whatever trailed the clock. A return from pause resubscribes to the same broadcast instance, so `js/watch/src/audio/decoder.ts` keeps the predecessor's ring without a join. The resumed picture lands on the live edge, and the audio that trails it by about 1.7 s is stretched back over about 5 s.

Evidence, `just test media --media-cases pause` in `test/interop/clients/js/media.ts`:

- `main` at 18fc33ecc: 3 of 4 runs fail the resume sync (median skew 200 ms, or 0 of 79 in sync).
- Parent of #8507 (ac73d9e0b): passes, median skew 0 ms (one run). On a host at load 30, four full `just test media` runs there passed 1, and the other 3 failed only `audio tone` (67 to 70 of 81 samples, the CI signature), never sync; the 11-11 head a4bc922c6 passed 4 of 6, failing the same way. #8507 itself (4262a0691) failed 4 of 4 on sync, median skew 400 to 600 ms after resume. So the CI step can stay red under load after this fix, from a tone flake older than #8507.
- Experiment, not committed: a same-instance resubscribe calls the decoder's `reset()`, so the ring joins the clock again. 4 of 4 runs pass, median skew 0 ms. One full `just test media` run also passed the pause and rejoin steps, then failed the later `live incumbent fallback` case, which was not investigated.

Decided 2026-11-12 (maintainer): ✅ snap to the live picture. Re-join the ring when a viewer returns from absence on the same instance, so resumed audio lands on the clock the way a tune-in does, and keep the 2 s bound for stalls. Rejected: stretch catch-up with #8507's time-stretch, which plays the lag back over about 5 s and costs the lip sync that #8031 fixed; relaxing the resume assertion to match it, since `js/watch/AGENTS.md` says to judge a player change by its lag first. Check it with `just test media --media-cases pause`.

Open for the implementer:

- Scope the re-join to a return from absence. The handover path (`#handover` in `decoder.ts`) also inherits the ring for a rendition swap and a fallback, which should keep their seamless handover. Check the fallback case.
- Native parity: `moq play` pauses through `rs/moq-audio`. Check whether its ring inherits the same way; `just test media` does not cover it.
