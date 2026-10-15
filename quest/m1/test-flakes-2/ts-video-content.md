# [L] TS compliance fails only on real loss

## Goal

CI's `TS compliance` step (`just test ts`, `.github/workflows/ts.yml`)
gives the same verdict on every run of the same code. Its decoded-content
checks (`test/ts/decode.py`) fail only when the exporter really loses or
corrupts content, never because of how the host scheduled the run. Each
frame the exporter drops is counted and logged, so a failure says what was
dropped and why. The checks stay as strict: a captured suffix that loses or
duplicates a picture or an audio frame still fails.

## Plan

### Evidence

CI: seen 2026-10-13 on #5471, #5481 and two unrelated branches within the
same hour, each passing on rerun. Then, from `gh run list --workflow ts.yml`
on 2026-10-14: since #5328 merged (2026-10-13 23:49 UTC), 4 of 41 completed
PR runs failed `video-content` ("decoded pictures are not an intact suffix of
the source"): #5446 at f6629aac8
([38506130438](https://github.com/moq-dev/moq/actions/runs/38506130438)),
#5148 at 5adfd84a6
([38502761750](https://github.com/moq-dev/moq/actions/runs/38502761750)),
`quest/m1/cli-duration-defaults` at e7e3eb1c4
([38502673714](https://github.com/moq-dev/moq/actions/runs/38502673714)), and
#5481's branch `quest/m1/catalog-wall-clock-precision` at 312ac6260
([38503267980](https://github.com/moq-dev/moq/actions/runs/38503267980)).
The first three do not touch TS; #5481 changed the shared clock this
pipeline reads, so keep it out of any argument that narrows the cause.
Only the default `just test ts` round failed; every later round passed in the
same runs. Main has no run yet: the workflow runs on matching PRs and a
nightly that had not fired. Failures on branches without #5471 or #5481
point to an older cause, but check a failing capture against those diffs
before ruling them out.

Every failing capture was 24531556 bytes against 23281544 in passing runs.
The difference, 1250012 bytes, is 1 s at the padded 10 Mb/s rate, so the
failing export started one 25-frame GOP earlier (group 1, not group 2). The
`--hrd` and `--headroom` rounds show the same one-GOP swing between runs but
do not run `decode.py`. Each failing run uploaded a `ts-failure` artifact
(`decode/*/frames.sha256`, `sub.log`), kept 7 days.

Local, on 2026-10-14, `test/ts/run.sh` on a host at load ~28 on 24 cores
failed 3 of 4 runs, in two other ways:

- `video-content`, once. The subscriber's `https://` race was won by
  WebSocket, then upgraded to WebTransport; the old session "did not drain in
  time" (GOAWAY timeout), and the exporter logged "current group evicted;
  skipping to next buffered group" (`rs/moq-mux/src/container/consumer.rs`)
  and one "frame missed its deadline". About 30 pictures across groups 10 and
  11 were missing, yet FFmpeg decoded the capture without an error.
- `audio-timing` ("capture has a gap or overlap in decoded audio"), twice,
  over WebTransport with no upgrade and intact video. Two or three AAC frames
  (43 to 64 ms) vanished mid-stream with no warning anywhere, and the
  exporter's release clock reported `dropped=0`.

So in each case the checks saw real loss. The flake is what makes the
exporter lose content, and the loss goes unreported.

2026-10-15: the same `video-content` failure blocked unrelated #5566 twice in
one run ([38725177110](https://github.com/moq-dev/moq/actions/runs/38725177110),
both attempts), and hit #5553 and #5148 again, so the rate is above the 4 in
41 estimate. In each of #5553's and #5566's `sub.log`, about 20 ms after the
video subscribe: `current group evicted ... group=1
error=Hang(Moq(Stream(Old)))`, and `relay.log` and `pub.log` show
`max_delay: 0ns` on `0.avc3`. The publisher serves group 1, group 2's first
frame lands about 17 ms later, `poll_drifted` judges group 1 zero behind the
edge, and it resets group 1's stream `Old` before the raised budget reaches
it through the relay. That confirms hypothesis 1's first candidate (analysis
on #5565). Capture size does not separate the cases at a 1 s `--delay`: a
passing local run captured 25156468 bytes, like a failing CI run, so use the
log signature.

### Hypotheses

1. Startup truncation (CI). The importer holds the catalog for its 2 s PCR
   measurement window (`rs/moq-mux/src/container/ts/mux_rate.rs`), so the
   exporter subscribes around group 2's IDR, and whether group 1 is still
   the newest is a wall-clock race. A group-1 join can then keep only a
   prefix of group 1 before skipping to group 2. Two candidates:
   - The live subscription starts with `max_delay = 0` and only relaxes to
     the budget after the first frame (`rs/moq-mux/src/container/source.rs`),
     so moq-net may expire group 1 partway through.
   - The jitter buffer's acquisition judges staleness per frame against its
     own read time (`judge`/`queue` in `rs/moq-mux/src/jitter.rs`). The held
     IDR can pass while the frames read after the tables were built are
     stale, and the track then waits for the next keyframe. Those drops are
     `Wait::Acquired`, which is not counted in `dropped` or logged.
2. Transport upgrade (local mode 1). The WebSocket-to-QUIC upgrade's handover
   evicts the current group. [#5484](https://github.com/moq-dev/moq/pull/5484)
   (open) removes the upgrade. Check whether a GOAWAY drain still loses the
   current group's tail, since drains remain.
3. Silent audio loss under load (local mode 2). Each AAC frame is its own
   group (`rs/moq-mux/src/container/ts/import.rs`), so a group expired by the
   latency budget, or skipped by the consumer, drops one frame with no count.

Ruled out from reading the code: picture duplication, timestamp rebasing, and
pre-IDR content in group 0 (the importer opens a group only at a keyframe).

### Steps

- Reproduce first. Download a `ts-failure` artifact from one of the CI runs
  above before it expires (about 2026-10-21) and map the captured pictures
  onto the source to confirm hypothesis 1. Locally, loop `test/ts/run.sh`
  beside a CPU hog, as above.
- Fail loud: every frame the exporter drops (acquisition, late, or a skipped
  or expired group) is counted and logged, and the harness prints that count
  beside a failing check.
- Fix each confirmed cause at its source. Recommended for hypothesis 1: an
  export starts at a whole group, so acquisition admits or drops a group
  entirely rather than emitting a prefix. Keep the zero-budget live-edge
  pick (`ExportSource::live`): starting with the full budget would let a
  join reach back to an older retained group, adding startup latency and
  replay, a user-visible change. Make the group it picks atomic instead.
  Each fix lands with a regression that fails without it, on a paused Tokio
  clock or the controlled clock, never wall time. The jitter and export
  tests in `rs/moq-mux` already run on mocked time.
- Keep the harness on its real-time path; this quest fixes the exporter, not
  the grader. If #5484 has landed, rerun on top of it before chasing
  hypothesis 2. Never loosen the comparison, widen a timeout or `--delay`, or
  add a retry.
- Open question for the implementer to put to the maintainer, once the causes
  are known: whether a frame dropped because the host starved the exporter
  past its latency budget fails `decode.py`, or fails a separate named check
  fed by the exporter's drop count. Recommended: a separate check, so
  `video-content` and `audio-content` mean corruption only.

Public API: none expected; the drop count may surface in the exporter's
summary. Wire: none.

## Closes

- [#5565](https://github.com/moq-dev/moq/issues/5565) - the TS compliance video-content flake

## Related

- [#5484](https://github.com/moq-dev/moq/pull/5484) - removes the WebSocket-to-QUIC upgrade behind local mode 1
- [Loss and recovery tests](/quest/m1/ts-reliability/recovery.md) - deliberate MoQ content loss, which this flake hits by accident
- [Controlled time](/quest/m1/time/README.md) - the clock the regressions should run on
