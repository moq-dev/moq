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

Not reproduced locally yet, so the next failure has to decide it. [moq-dev/moq#9765](https://github.com/moq-dev/moq/pull/9765)
made that failure decisive: on a failed gst cell the harness now prints moqsrc's whole log, with
its `moq_net::lite` subscriptions at debug and its `moq-src` GStreamer category at level 6 (where
a rendition that fails to subscribe or read is reported), and says whether gst-launch exited or
hit the deadline. The relay already logs `moq_net::lite::publisher` at debug, which shows what it
served the gst session. When the next failure lands, read those two logs for the gst session's
`0.avc3` subscription, then reproduce that path in a failing test and fix it.

This is not the cause [moq-dev/moq#9079](https://github.com/moq-dev/moq/pull/9079) fixed
(merged 2026-11-21, before both failures): moqsrc's 1 s default `max-delay` aging out the finished
tail.

### The CI failure

In run 45675453343, moqsrc (moq-lite-06 over WebTransport) logged the broadcast `online` at
18:56:34.52, a quarter second after the publisher's session started, kept it for the publisher's
whole 10 s linger until `offline` at 18:56:44.31, and pushed no buffer: gst-launch printed no
event or caps line, so no `video_0` pad ever streamed. At `420b8dcb0d` the harness printed only
moqsrc's INFO lines and logged the relay's `moq_net::lite::subscriber` alone at debug, so the
relay log shows its upstream subscriptions to the go publisher (conn 188, moq-lite-07-wip) but
not what it served the gst session (conn 189, moq-lite-06). Kept here, since the artifact expires
2026-12-02 (ids and timestamps trimmed, `b` is `finish-go-19687-836/fixture.hang`):

```text
18:56:34.274858Z  INFO conn{id=188}: session accepted publish=finish-go-19687-836
18:56:34.275641Z  INFO conn{id=188}: negotiated version=moq-lite-07-wip transport=webtransport
18:56:34.284444Z DEBUG conn{id=188}: sub::routed: announce route=b
18:56:34.344711Z DEBUG conn{id=188}: sub: subscribe started id=0 b track=catalog.json
18:56:34.454560Z DEBUG conn{id=188}: sub: finished track released b track=catalog.json
18:56:34.469658Z DEBUG conn{id=188}: sub: subscribe started id=1 b track=0.avc3
18:56:34.471226Z DEBUG conn{id=188}: sub: subscribe started id=2 b track=catalog.json
18:56:34.505872Z  INFO conn{id=189}: session accepted request.role=Some(Subscriber) subscribe=finish-go-19687-836
18:56:34.506617Z  INFO conn{id=189}: negotiated version=moq-lite-06 transport=webtransport
18:56:34.532027Z DEBUG conn{id=188}: sub: finished track released b track=0.avc3
18:56:34.549217Z DEBUG conn{id=188}: sub: subscribe started id=3 b track=tone
18:56:34.557630Z DEBUG conn{id=188}: sub: subscribe started id=4 b track=0.avc3
18:56:34.571478Z DEBUG conn{id=188}: sub: finished track released b track=tone
18:56:34.585852Z DEBUG conn{id=188}: sub: finished track released b track=catalog.json
18:56:34.610127Z DEBUG conn{id=188}: sub: finished track released b track=0.avc3
18:56:44.298360Z DEBUG conn{id=188}: sub: source closed err=closed
18:57:45.825853Z  WARN conn{id=189}: connection closed id=189 err=cancelled
```

And moqsrc (`finish-go-gst.out`, INFO only, no buffer or event line):

```text
18:56:34.491280Z  INFO moq_tokio::client: connected version=moq-lite-06 transport=webtransport
18:56:34.512437Z  INFO gstmoq::source::imp: waiting for broadcast to be announced path=b
18:56:34.519615Z  INFO gstmoq::source::imp: online path=b instance=Some(Instance("ecbb96a1d7e62c2028b4e477d1a24805", "minted"))
18:56:44.306453Z  INFO gstmoq::source::imp: offline, holding pads until it returns path=b
```

Only moqsrc reads `tone`, so `id=3` and `id=4` are its renditions: it read the catalog and
subscribed both, and the relay received the whole `0.avc3` tail on `id=4`. Unlike every
passing row (python, cpp), where the relay's catalog copy finished before moqsrc's track
subscriptions, here moqsrc read the catalog from an in-flight upstream copy (`id=2`) whose end
arrived about 30 ms after its track subscriptions.

Only moqsrc reads `tone`, so `id=3` and `id=4` are its renditions: it read the catalog and
subscribed both, and the relay received the whole `0.avc3` tail on `id=4`. Unlike the passing
python and cpp rows of the same run, where the relay's catalog copy finished before moqsrc's track
subscriptions, here moqsrc read the catalog from an in-flight upstream copy (`id=2`) whose end
arrived about 30 ms after its track subscriptions.

### What did not reproduce it (2026-11-27)

On a 32-core host at a load average of 75 to 95, 0 failures in 76 `finish * -> gst` cases:
`just test interop --finish` with go, python, and rust publishers alone and beside rust, python,
and go readers; `--all` three times; `--versions moq-lite-06,moq-lite-07-wip` five times; and ten
runs with a scratch harness change that sent the other readers' `read` 0 to 400 ms after gst
started, so their relay copies were in flight when gst joined.

The CI ordering, with finished tracks and the catalog's end reaching moqsrc after its frame, also
played all ten frames every time in scratch moqsrc tests: 35 runs in process with the end 0 to
50 ms after the frame, and 66 runs through a relay hop over moq-lite-06 with the end 0 to 200 ms
after gst started. So moqsrc's catalog-close path (`follow_catalog`, `Follower::run`) handles that
ordering; what remains is something the scratch tests lacked, such as moq-relay serving a
moq-lite-06 reader from a moq-lite-07 copy, or WebTransport.

Public API: none. Wire: none.

## Waiting

- moq-dev/moq: the next `finish * -> gst` failure, in a scheduled Interop run or a local
  `just test interop --all`, with the moqsrc log this harness now prints; check with
  `gh run list --repo moq-dev/moq --workflow interop.yml --status failure` and search the failed
  job's log for `finished frames before`.
