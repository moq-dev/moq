# [M] The gst finish reader gets the finished tail every run

## Goal

The interop finish lane's GStreamer reader (`run_finish_gst` in `test/interop/interop.sh`) reads
all ten frames of every publisher's finished tail every run, alone and under load, fixed at the
cause. Today it intermittently gets none, which fails its own cell and, since the harness then
withholds the publisher's acknowledgement, every other cell of that publisher's row as
`reader failed: gst`.

Seen twice:

- Scheduled Interop [45675453343](https://github.com/moq-dev/moq/actions/runs/45675453343)
  (2026-11-25, at `420b8dcb0d`): `finish go -> gst (exited 1)` with
  `gst: 0 of 10 finished frames before the deadline`, so all nine `finish go ->` cells failed.
  The rust, python, cpp, and js rows passed, gst included.
- A local `just test interop --all` on [moq-dev/moq#9553](https://github.com/moq-dev/moq/pull/9553)
  (merge `43499f00b0`): the same gst signature failed every `finish python ->` case, and in the
  same run the media matrix's `go -> js-native-node` failed at 5 s of 30 s. The rerun passed, and
  that PR touches neither `rs/moq-gst` nor the harness. Its Validation section is the only record.

## Plan

In the CI run, moqsrc (moq-lite-06 over WebTransport) logged the broadcast `online` at
18:56:34.52, a quarter second after the publisher's session started, kept it for the publisher's
whole 10 s linger until `offline` at 18:56:44.31, and pushed no buffer. Every other reader of that row printed
`finish clean frames=10`. The 20 lines of gst output the harness prints are INFO only, so they
don't show whether moqsrc subscribed to `0.avc3` or what came back.

This is not the cause [moq-dev/moq#9079](https://github.com/moq-dev/moq/pull/9079) fixed
(merged 2026-11-21, before both failures): moqsrc's 1 s default `max-delay` aging out the finished
tail. The harness now passes the whole read phase as `max-delay`, unless that setting fails to
reach the subscription.

1. Read the relay log in the CI run's `interop-failure` artifact (1.4 GB, expires 2026-12-02):
   the relay logs `moq_net::lite::publisher` at debug, so it shows which subscriptions the gst
   session opened for `finish-go-19687-836/fixture.hang` and what was served.
2. Reproduce: loop `just test interop --finish --publishers python,go,rust --subscribers gst`
   through `quest gate` under CPU load, with `RUST_LOG=debug` and `GST_DEBUG=moq-src:6` for
   gst-launch, across `--versions moq-lite-06,moq-lite-07-wip`. #9079 could not reproduce its
   cause with load alone; a scratch delay of 0.5 to 2 s before `gst-launch` did, so vary when gst
   joins relative to the publisher's finish too.
3. Likely places to look: moqsrc's catalog-close path, where a late joiner reads the finished
   catalog's one frame and its end together (`follow_catalog` and `Follower::run` in
   `rs/moq-gst/src/source/imp.rs`), and the relay serving a finished track's tail to a fresh
   subscription.
4. If nothing reproduces, record the counts here and make `run_finish_gst` keep moqsrc's debug
   log on failure, so the next CI hit decides it.

Public API: none. Wire: none.
