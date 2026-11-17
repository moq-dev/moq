# [S] A WHEP pull chained into an export stops cleanly

## Goal

Stopping `moq import webrtc --connect <whep> -- export rtmp --connect <rtmp>`
with SIGTERM exits 0 and logs no warning about its own tracks. The RTMP export
does not fail with `the source's clock runs +59.8 ppm off ours, past the 30 ppm
the output's may follow`. The WHEP ingest's tracks (`0.opus`, `1.avc3`,
`catalog.json`, `catalog`) end with `finish()` or `abort()` instead of logging
`track::Producer dropped without finish() or abort()`. Non-goal: orderly
broadcast ends while the process keeps running, which
[export-close](/quest/m3/export-close.md) owns.

## Plan

Found 2026-11-17 while validating
[moq-dev/moq#8997](https://github.com/moq-dev/moq/pull/8997) on the built
binary. Its PR body's Validation section has the three-process loopback setup
(an ffmpeg source through `export webrtc --listen`, the chain, and an RTMP
sink), and the stop shows both symptoms in some runs. The maintainer picked
this follow-up when picking #8997 to merge.

- **Clock drift on stop.** The error comes from the drift estimate in
  `rs/moq-mux/src/jitter.rs` (`Error` in `rs/moq-mux/src/error.rs`), judged over
  only the few seconds the chain ran, while the ingest's timeline was still
  settling from arrival time onto RTCP sender reports (`IngestClock` in
  `rs/moq-webrtc/src/session.rs`). First find out whether the stop causes it
  (for example, a timestamp jump while draining) or whether a short run would
  trip it anyway. Then fix whichever is at fault: the ingest timeline or an
  estimate that lacks enough samples to judge drift.
- **Unfinished tracks.** `Session::run` aborts the sink only when its loop
  returns. A SIGTERM drops the pull's future instead, which drops the
  `IngestSink` and its bridges unfinished. Finish or abort them on drop, the
  way the rtsp import closes its catalog after its select.

## Related

- [Container export ends cleanly](/quest/m3/export-close.md) - an export's orderly end when its broadcast closes
- [Interop stops publishers cleanly](/quest/m3/interop-shutdown-warnings.md) - the same unfinished-producer warning in the CLI FETCH round
