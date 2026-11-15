# [L] Firefox audio resumes with the picture

## Goal

Nightly's `Media (firefox player)` job passes its `after resume` window: after
a pause, audio is audible within the window's first samples, as in Chromium,
instead of staying silent for about half a second after the picture resumes.

## Plan

Nightly [43601796176](https://github.com/moq-dev/moq/actions/runs/43601796176)
(11-14) and [43250182795](https://github.com/moq-dev/moq/actions/runs/43250182795)
(11-12) failed the same way: the window's first samples are silent with the
audio byte counter flat, then `skipping missing groups: track=audio 1165 ->
1258` (1152 -> 1248) fires and audio plays in sync.

Reproduced 4 of 4 with every process pinned to two cores (the hosted ARM
runner is slower than four local x86 cores, which pass):

```sh
HARNESS_KEEP=1 RUST_LOG=moq_net::lite::publisher=debug taskset -c 4-5 \
  nix develop --command just test media --media-engines firefox --media-cases rename,pause
```

`pause` alone never fails: without `rename` first, the fixture stops
producing while unsubscribed, so no backlog builds. With it, the fixture
keeps producing through the pause, and the relay log shows the resume:

1. The relay answers the resubscribe from its cached copy, whose upstream
   subscription it cancelled when idle at the pause: it serves that copy's
   tail (the groups before the pause, about 2.6 s old) at once, 100 to 500 ms
   before the renewed upstream subscription returns its first live group.
2. The player's new audio consumer starts at that tail, delivers it, and so
   reads mid-stream when the live groups land past a hole of about 95 groups
   (1.9 s, inside the subscription's 2.08 s max delay).
3. The upstream backlog filling that hole arrives newest first, so the group
   the consumer waits on lands last. Starved of CPU, the relay took 80 to
   650 ms to serve it, longer than the hole wait (delay plus lookahead), so
   the consumer sat silent for the whole wait and skipped.
4. The audio decoder rejoins the live edge on resume and drops everything
   behind its clock, so the wait buys nothing: the backlog would be discarded.

Neither hop alone is the fix. Resolving the relay's start against its source
(1) still leaves a backlog inside the budget, which the consumer waits on
from its oldest group (3); a consumer that starts at the live edge less its
reach (3) still starts on the relay's stale tail, which arrives first and,
while the source has not answered, is all the relay's `resolve` can report
as its largest (`poll_live` pending falls back to the cached largest) (1). The same wait slows any cold join onto a
relay holding a full max-delay backlog over a slow link, not just a resume.

The transport is not the bottleneck: qmux over WebSocket grants 10,000 streams
and a 4 MiB window, and the relay's own serve rate is what stretched.

Do not widen the test's window or agreement.

Kept in m0 by the maintainer (2026-11-15): ✅ m0 (rejected: m1, drop).

## Waiting

- Maintainer: pick the fix, since it reverses the documented consumer start
  (`ConsumerProps.warmup`: delivery starts at the lowest group that arrived,
  and a backlog within the max delay plays):
  1. (recommended) Both hops. The relay resolves a lapsed copy's start and
     largest against its source's live largest before serving the cached tail
     (`lite/publisher.rs` `resolve`, already reading `poll_live`), and both
     consumers (`js/hang` `Consumer`, `rs/moq-mux` `container::Consumer`)
     treat a served start as a floor, not the cursor: the start waits for the
     publisher's largest group, then begins at the group covering the live
     edge less the reach, discarding the backlog below it. Fixes resume and
     cold join alike.
  2. Player only: the audio decoder's rejoin after a stop opens its consumer
     at the live edge, like Rust's `Consumer::live()`. Smaller, but the
     stale tail still arrives first and names the largest, so it starts there
     and the hole remains: it does not fix the failure without the relay change.
  3. Keep the design and run the Firefox media job on a larger runner. Hides
     a real startup cost on slow viewers, so not recommended.
