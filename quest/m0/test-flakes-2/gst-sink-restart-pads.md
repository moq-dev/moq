# [S] moq-gst's sink-restart pad test survives load

## Goal

`source::imp::session_tests::a_moqsink_restart_keeps_pads_when_caps_arrive_at_different_times`
passes reliably under host load, fixed at the cause rather than retried.

## Plan

Seen 2026-11-07 on [moq-dev/moq#8122](https://github.com/moq-dev/moq/pull/8122)'s
Check ([job](https://github.com/moq-dev/moq/actions/runs/42341390803/job/128309636226)),
a PR that doesn't touch moq-gst: `assertion failed: playing.iter().all(|pad|
held.contains(pad) && pad.starts_with("video_"))` at
`rs/moq-gst/src/source/imp.rs:5830`, followed by a GStreamer-CRITICAL that
`moqsink0` was disposed in PAUSED instead of NULL. Reproduce under load first,
then find whether the test reads the pad set before the restart settles or the
source really drops a pad.
