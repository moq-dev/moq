---
name: verify-failover
description: Model-check failover along a chain of relays (`rs/moq-net/verify/failover/FailoverChain.tla`) with TLA+. Use when a change touches how a relay's front switches routes or ends a track (`model/front.rs` `track_ended`, `model/origin.rs` `Action::Finish`, `park`), how a switch asks the new copy (`model/resume.rs` `Reader::sync`, `backfill_gaps`, `Producer::subscription`), how a group continues on another copy mid-read (`Recover`, `Fetching`), or when a failover bug spans more than one hop and needs every ordering of deaths, switches, and frames checked rather than a random sweep.
---

# Verify failover along a relay chain

`rs/moq-net/verify/failover/FailoverChain.tla` models one track served to a client `C` through relay `E`, which starts on relay `A` and may fail over to relay `B`; both pull from the origin `O`, and with `Mesh` `B` may also route through `A`. Each layer is abstracted to the steps its own model checked, so the state stays small: a route change is one `Switch` (`verify-route`), a switch asks the new copy from where the readers got to and each reader hands a group out once and ends once nothing it replaced may still deliver (`verify-resume`), and a group handed out crosses a hop unless its session dies (`verify-subscribe`, with an hour's budget). What the chain adds is frames: a group whose copy fails or stalls mid-read continues on another copy at the same frame (`Recover`), from a copy its relay holds, from the serving copy, or by fetching the rest from the serving route, which asks its own upstream in turn.

## Rules

Each rule is an INVARIANT, except `Settles`, which is TLC's deadlock check: every path must end with the read over (`Finished`); the harm says why it matters.

| Rule | Holds when | Harm if broken |
| --- | --- | --- |
| `CleanEndIsComplete` | The client's track ends cleanly only with every group handed out. | The viewer loses media silently at the end. |
| `CleanEndIsDeclared` | The client's track ends cleanly only after the origin ended it. | A failover reads as the broadcast ending. |
| `NoSkipWhileHeld` | A group the client started fails only once `E` has no route left. | A group is cut short while another path holds the rest. |
| `SettledIsWhole` | A finished read holds every frame of every group. | A group ends short of its last frame. |
| `Settles` (deadlock) | Every path ends with the read over, or, once the client left, with no relay holding a copy. | Playback hangs, or a relay keeps its upstream subscribed after the viewer left. |

## Run

```sh
quest gate just rs tla moq-net/verify/failover/FailoverChain.tla
quest gate just rs tla moq-net/verify/failover/FailoverChain.tla -config rs/moq-net/verify/failover/Leave.cfg
quest gate just rs tla moq-net/verify/failover/FailoverChain.tla -config rs/moq-net/verify/failover/Mesh.cfg -simulate num=200000 -depth 100
```

The first checks every rule with `E` failing over between `A` and `B`, one death, and one switch per relay, with a client that stays, so a deadlock is a playback hang: about 2.0M states. The second lets the client leave at any point, so a deadlock is a relay still holding a copy after it left; leaving is always possible there, which is why it cannot stand in for the first: about 2.2M states. Each takes under a minute and passes at the sync commit. The third adds `B`'s route through `A`, so the route also moves at the middle hop, after `B`'s track ended too (`Action::Reroute`). It is too large to exhaust (past 139M distinct states at depth 38 after an hour, its queue still growing), so its `Simulate:` line samples 800k random behaviors in about 7 minutes, and Nightly samples new ones each run. An exhaustive run passed every state through depth 37. Before the reroute, a sample that size missed the depth-31 hang that BFS found after about 48M states, so a passing sample is weak evidence: check a change at the middle hop by running it without the `-simulate` arguments for as long as it takes.

## Reading a failure

Follow Reading a failure in `rs/moq-net/verify/README.md`. Replay the path on the real stack in `rs/moq-net/tests/integration/failover_chain.rs`: relays are `origin::Producer`s linked over the memory transport (`link`, `priced_link`, a cost to make a route win), a relay or hop dies by aborting its sessions, a group read past a failed copy is what makes `Recover` fetch, and time is simulated. Pin a path the code follows as a `residual_*` test with its own config so the others still pass.

## Coverage

Sync commit: `88572c583e2f64c7f5fc9bfa59df90706308497e`. Run `git log 88572c583e.. -- rs/moq-net/src/model/resume.rs rs/moq-net/src/model/front.rs rs/moq-net/src/model/origin.rs rs/moq-net/src/model/track.rs` to see what changed since. Follow Coverage in `rs/moq-net/verify/README.md` after a change to a covered function.

| Code | Model |
| --- | --- |
| `rs/moq-net/src/model/resume.rs` `Reader::sync`, `backfill_gaps`, `Producer::subscription` | `ResumeAsk`, `Demand`, `Switch`, `Subscribe` |
| `rs/moq-net/src/model/resume.rs` `Reader::take`, `poll_end` | `HandOut`, `Drained`, `EndOk` |
| `rs/moq-net/src/model/resume.rs` `Reader::let_go`, `Copy::retains`, `Producer::park` | `LetGo`, `Park` |
| `rs/moq-net/src/model/resume.rs` `Recover::wants`, `poll`, `held`, `poll_serving` | `Recover`, `Holders`, `Exhausted`, `Abort` |
| `rs/moq-net/src/model/resume.rs` `Recover::poll_fetch`, `Fetching`; `rs/moq-net/src/model/track.rs` `fetch_group` | `Ask` |
| `rs/moq-net/src/model/front.rs` `track_ended`, `Action::Finish`; `rs/moq-net/src/model/origin.rs` `TrackIo::end` | `FrontFinish`, `Whole` |
| `rs/moq-net/src/model/front.rs` failover and its unroutable end, `Action::Reroute` | `Switch`, `FrontFail` |

## Not covered

Routing is abstracted to any live upstream winning, so feasibility, ROUTE_END, and announce order are `verify-route`'s; how a reader picks among its copies, bounds replaced copies, and orders groups is `verify-resume`'s; drops, skips, and re-offers on a hop are `verify-subscribe`'s. The client's track fails only through `FrontFail`, so whether one dead relay can end playback is that step's guard, not a rule. Every hop has an hour's budget, so nothing goes stale or expires. The origin writes each group whole and never fails, `E` never dies, a dead relay never returns, and a relay's cache is dropped when it parks. A fetched group holds its head from frame 0 (`lend_head`), and compressed groups are left out. A pass covers two groups of two frames, one death, one switch per relay, and one client.
