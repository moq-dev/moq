# [M] A reader refused as unroutable reads again on lite-07

## Goal

On lite-07, a reader whose relay still shows its broadcast keeps reading after
a link reopens or a relay crashes, as the release (lite-06) does. Today, about
one run in twenty, a relay answers a reader's live subscription with
Unroutable while it keeps announcing the broadcast unchanged. The reader's
track strands, waiting for a route change that never comes, and starves for
the rest of the run without an error.

No reader starves in
`just test mesh-replay --mesh test/mesh-replay/ring4.toml --script test/mesh-replay/reopen.toml --broadcasts 32`
or in the nightly ring meshes, once `[[known]]` no longer names this quest in
`test/mesh-replay/script.toml`.

## Plan

Found by [the mesh replay](/test/mesh-replay/README.md). On main after
[roll-echo-stall](https://github.com/moq-dev/moq/pull/9174) and
[reopen-starve](https://github.com/moq-dev/moq/pull/9179), 19 of 20
`reopen.toml` runs (ring4, 32 broadcasts, debug build) passed. The failing run
starved 2 reader paths.

What that run's logs show. `b`'s reader followed `d/0` and `d/3` through `a`.
The `a`-`b` link paused, and its session idle-timed out 10s later. At that
moment, relay `b` reset the reader's live subscriptions for exactly those two
paths with Unroutable (`subscribe error ... err=unroutable` in the reader's
log). The reader's follow logged no end and stayed online. Relay `b` never
withdrew or updated `d/0`, so the reader's front, which strands a track
refused as unroutable until its table moves (`model/front.rs`
`track_ended` and `refuse`, `Track::stranded`, `unstrand`), never asked again.
The other 30 paths at `b` switched to the copy through `c`. That copy handed
out about one group every 5s until the `a`-`b` link returned, then fed every
reader again.

Reopen-starve's first plan named this strand: a refused track waits for
its table to move. #9179 removed the refusal between relays (a request follows
a claim only through a neighbour that holds it). The strand remains one hop
down, between a relay and its client.

Open, and why it could not be settled yet:

- Why relay `b` ended those two tracks Unroutable. The run had info logs only.
  Candidates: `Front::selected` finding no candidate (#9179's `holds` filter
  leaves none while a neighbour's copy of the claims lags its route) with no
  orphaned claim to wait on, so the front ends `Unroutable`. Or the publisher's
  node filter refusing. Reruns with debug logs on `lite::publisher`,
  `lite::subscriber`, and `model` failed only on hosts saturated by other
  work (100% CPU, relays at 5-30% of a core). There the measuring reader
  itself starved on loopback, so those runs show nothing about this bug.
- A mock-time reproduction. A ring4 integration test on `peer_with_latency`
  links passed: 8 to 32 broadcasts from `d`, readers on `a`, `b`, and `c`
  over their own sessions, one link cut or paused. It also passed with the
  cut swept from 0 to 1.5s after the readers start. The window needs timing
  the mocked clock does not hit.

Recommended fix, pending the cause: a hop must not answer a live subscription
with Unroutable while it keeps showing that broadcast available. Either the
front waits for a way as it does for an orphaned claim (`Front::waiting`), or
the hop updates its announcement so the downstream's table moves. Fixing only
the client to re-ask on a quiet table would need a timer and could loop on a
standing refusal. Rerun `/verify-failover`, since either changes the front
lifecycle it covers.

Next steps: capture one failure with debug logs on a host that is not
saturated, and record per-relay CPU with it to tell load apart from logic.
Find which path in relay `b` produced the Unroutable. Write the test at the
lowest layer that shows it, likely a front unit test in `model/front.rs`.
Then fix it and rerun `reopen.toml` at least eight times.

Pauses are not part of this quest. A debug build pauses about as long on
lite-06 as on lite-07 under the same load: link 13-14s, relay reopen 24-35s,
kill 25-30s. A release build pauses the same on both versions: link 11-12s,
relay reopen 17-19s, kill 20-23s. The release 0.17.17 binary pauses 10s, 16s,
and 19s at a lower load.

The lite-07 cut waits on it, as decided for reopen-starve on 2026-11-19
(under the maintainer's delegation for lite-07 routing calls): it is a
starved reader after a link flap or a crash, which a deployment sees daily.

Not a wire change.

## Related

- [reopen-starve](https://github.com/moq-dev/moq/pull/9179) - a request
  refused before the neighbour's claims returned, and never asked again
- [roll-echo-stall](https://github.com/moq-dev/moq/pull/9174) - a time start a
  shared copy never places
