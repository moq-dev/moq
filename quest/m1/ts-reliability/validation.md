# [L] TS validators run as automated tests

## Goal

TSDuck, FFmpeg, and the repository's TS graders assert the supported output
profiles in repeatable CI tests. A green run means the expected content was
decoded and the named timing and structure checks passed, not merely that a
tool parsed a file or returned zero.

## Plan

Required by the maintainer in the 2026-10-11 audit: every TS validation is
automated, including any additional validator introduced later. Use the pinned
Nix tools and share local `just test` recipes with CI. Reuse `test/ts` rather
than introducing a second harness.

Current gaps:

- `run.sh` uses FFmpeg to generate sources but never fully decodes the
  exported audio/video. `ffprobe_opus_channels` checks metadata only.
- `just test ts --pair` and `table-anchor.py` exist but no workflow invokes
  that arm. Add a scheduled gate at minimum.
- `interop.yml` triggers for the harness but not `rs/moq-mux/**`. Relevant
  mux, catalog, and TS CLI changes need the short integration gates.
- Plain captured runs do not reject a failed publisher, and the subscriber
  is reaped without its status contributing to the verdict. A partial file
  can pass analysis after the pipeline has failed.

Add a reusable full software-decode grader for supported clean profiles,
starting with the existing closed-GOP H.264/AAC generator. FFmpeg must fail
on decode errors, decode every expected stream, and produce nonzero video
frames and audio samples where the source has them. Preserve diagnostics and
per-stream counts. A successful probe or concealment-tolerant decode is not
the assertion. Prove the grader rejects malformed coded payload and a missing
expected stream with automated controls that fail for the intended reason.

The first declared profile also compares decoded content against the source
over the admitted interval. Match video frames and aligned audio samples rather
than requiring identical demux packet boundaries. Account explicitly for
startup, decoder priming, and end trimming; never loosen an existing duration
check to accommodate missing content. A grader that only checks decode
acceptance and nonzero progress must report that narrower result until the
source comparison passes. Open-GOP acquisition expectations land
with the existing leading-pictures work.

Keep TSDuck's structural checks and explicit PCR-accuracy threshold, the
byte-schedule grader, and live release timing as distinct assertions. A CBR
byte-position PCR check is not a wall-clock jitter measurement. VBR and MPTS
with independent clocks need their own applicable timing model before they
can be labelled covered. Retain process failures, reports, tool versions,
input fixtures or reproducible generation parameters, and failing output as
CI artifacts.

The custom T-STD model currently excludes the PSI system buffer, refuses
several layouts/codecs, and borrows AAC's buffer size for Opus. Report these
limits by profile. Coordinate system-buffer controls with `tstd/psi-runs`
and MB/AAC controls with the existing m2 quest; do not duplicate them or call
the current model complete. Correct the blanket claim in the harness docs
that no maintained tool implements T-STD: commercial analyzers advertise
it, while no suitable maintained open-source replacement was established.

Independent failure reporting landed in #5297; keep its workflow shape, and
ensure an earlier unrelated failure cannot suppress these tests. All new
controls have CI invocations too.

## Related

- [TS duration fidelity](/quest/m0/ts-duration-fidelity.md) - fixes the base capture regression without weakening the duration assertion
- [T-STD controls](/quest/m2/tstd-controls.md) - MB and AAC positive/negative coverage
- [PSI system buffer](/quest/m1/tstd/psi-runs.md) - system-buffer proof belongs with the PSI fix
- [Open-GOP leading pictures](/quest/m1/open-gop-leading-pictures.md) - acquisition trims change the decoder oracle
- [Loss and recovery](/quest/m1/ts-reliability/recovery.md) - extends these graders to declared omissions and recovery
