# Two-relay 1+1 source-failover drill

`just test failover` stands up two meshed relays, two publishers of the same
broadcast, and three subscribers, then kills the active publisher and checks that
the surviving standby takes over.

This is the end-to-end counterpart to the routing unit tests in
`rs/moq-net/src/model/origin.rs`. Those cover the model. This covers the wiring:
two real `moq-relay` processes, real `moq import ts` publishers whose tracks are
created lazily by the demuxer, and the QUIC idle timeout in the loop.

## Topology

```text
pubA ──▶ relayA(:4443) ◀──cluster──▶ relayB(:5443) ◀── pubB
          ▲   ▲                          ▲
        sub1 sub2                       sub3
```

`pubA` and `pubB` publish the same broadcast with the **same `--origin` id**,
which is what declares them interchangeable sources rather than different
content. (`moq --origin` arrives with
[#2473](https://github.com/moq-dev/moq/pull/2473); on a build without it the
drill exits with a diagnostic rather than reporting a bogus zero-byte failure.)

`sub3` matters more than it looks. It subscribes on relayB, which has no local
publisher at first, so relayB has to carry the broadcast across the mesh from
relayA. That makes relayA a hop in relayB's own route, and therefore the peer
that per-peer announce selection has to treat specially: relayB must not
advertise the route that runs *through* relayA back to relayA, but it must
advertise the local `pubB` standby once that exists. Without `sub3` the
interesting case never arises.

## What is checked

| Check | Proves |
|---|---|
| **1, failover** | After `pubA` is killed, `sub1` on relayA resumes. relayA had already been told about the `pubB` standby and reselected onto it. |
| **2, standby join** | `sub3` on relayB keeps its subscription when `pubB` attaches locally. A standby wins dispatch the moment it attaches, which is *before* a real publisher has created every track, so a per-track refusal must not abort the whole subscription. |

Check 2 is a regression test for a bug that only appears with real publishers.
`moq import` announces its broadcast on connect and creates each track only once
its demuxer reaches it, so a freshly attached standby legitimately cannot serve
some tracks yet. A model-level standby accepts every track request immediately
and never reproduces it.

## Three things about the harness that are load-bearing

Changing any of them turns a real result into a meaningless one, quietly. Each
cost a wrong conclusion at least once, so each is commented where it is set.

**The observation window is derived, not chosen.** Killing a publisher never
sends `CONNECTION_CLOSE`, so the relay keeps serving the dead source until the
**QUIC idle timeout** expires, 30 s by default. The relay logs nothing at all
between the kill and `connection closed err=timed out`, and only then can it
reselect. **A grading window shorter than that budget cannot pass on any build.**
So the drill kills the active publisher at t=32 and grades at least 20 s *after*
the budget elapses. Recovery time is dominated by detection, not by the reselect:
expect `sub1` to resume roughly one idle timeout after the kill (30 to 33 s
measured). `--idle` pins a lower timeout on the relays and shortens the run with
it; it must stay above the keep-alive interval or healthy sessions will flap.

**The kill is atomic.** The publisher pipeline is SIGKILLed in one pass, because
killing `tsp` first would leave `moq import` reading a truncated stream plus EOF.
It would then shut its broadcast down *cleanly*, the relay would unannounce
immediately, and the drill would be grading a graceful detach rather than a
source failure. The two paths behave nothing alike: one waits out an idle timeout
and fails over, the other unannounces at once, and on that path the subscriber's
`export ts` currently dies with `TS track layout changed after PAT/PMT was
emitted` instead of switching to the standby.

**The standby joins early, and that is deliberate.** The two publishers replay
independent copies of the same clip *from its start*, so the standby's media
timeline lags the active one by roughly its join delay. On the splice the
subscriber's muxer has to wait for the new source's timestamps to overtake the
last ones it wrote, and that wait scales one-for-one with the join delay (join at
t=4 costs under 2 s, t=10 costs 9 s, t=20 costs 18 s). It is a property of this
harness, not of the relay, which reselects in the same millisecond the standby
connects. A real 1+1 pair shares a timestamp-aligned feed and does not pay it.
Hence the default join at t=4, and hence check 2 only warns when a stall exceeds
what that offset explains.

## Usage

```bash
just test failover                  # generated clip, 30s detection budget (~90s)
just test failover --idle 10s       # faster run (~70s)
just test failover --source cap.ts  # publish a real capture instead
just test failover --keep-logs      # keep the work dir (relay logs, sizes.csv)
```

Requires `cargo`, `ffmpeg`, `curl`, `pgrep`, `pkill`, and TSDuck's `tsp` (for PCR
pacing).

Environment overrides: `FAILOVER_PORT_A`, `FAILOVER_PORT_B`, `FAILOVER_PROFILE`
(`debug`/`release`), `FAILOVER_ORIGIN`, `FAILOVER_PRE` (standby join time),
`FAILOVER_KILLA` (when the active source is killed).

## Debugging a failure

`--keep-logs` prints the work dir, which holds both relay logs, every client log,
and `sizes.csv` (per-second byte counts for `sub1` and `sub3`).

`RUST_LOG=moq_net=debug` makes the announce decisions visible, which is usually
what you want:

- `no advertisable route for this peer exclude_hop=…`: relayB correctly
  advertising nothing while it is merely carrying via relayA.
- `announce broadcast=… hops=2`: relayB advertising the local standby to relayA
  once `pubB` joins. This is the line that must appear at the join; without it
  there is nothing for relayA to reselect onto.
