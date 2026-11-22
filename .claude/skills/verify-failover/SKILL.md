---
name: verify-failover
description: Model-check failover along a chain of relays (`rs/moq-net/verify/failover/FailoverChain.tla`) with TLA+. Use when a change touches how a relay's front switches routes or ends a track (`model/front.rs` `track_ended`, `model/origin.rs` `Action::Finish`, `park`), how a switch asks the new copy (`model/resume.rs` `Reader::sync`, `backfill_gaps`, `Producer::subscription`), how a group continues on another copy mid-read (`Recover`, `Fetching`), or when a failover bug spans more than one hop and needs every ordering of deaths, switches, and frames checked rather than a random sweep.
---

# Verify failover along a relay chain

`rs/moq-net/verify/failover/FailoverChain.tla` models one track served to a client `C` through relay `E`, which starts on relay `A` and may fail over to relay `B`; both pull from the origin `O`, and with `Mesh` `B` may also route through `A`. Each layer is abstracted to the steps its own model checked, so the state stays small: a route change is one `Switch`, a switch splits the subscription at one boundary, the largest group its relay holds or a reader got: it asks the new copy from there, past the frames of that group its relay holds, which it lends the new copy, and caps every copy it replaces there, stopping that group at the frame the new copy resumes it (a Position end); each reader hands a group out once and ends once nothing it replaced may still deliver (`verify-resume`), and a group handed out crosses a hop unless its session dies (`verify-subscribe`, with an hour's budget). What the chain adds is frames: a group whose copy fails or stalls mid-read continues on another copy at the same frame (`Recover`), from a copy its relay holds, from the serving copy, or by fetching the rest from the serving route, which asks its own upstream in turn. A relay whose track ended keeps moving the reads that still hold it (`Switch` after `FrontFinish`) and forgets the track only once nothing reads it (`Forget`, the linger).

## Rules

Each rule is an INVARIANT, except `Settles`, which is TLC's deadlock check: every path must end with the read over (`Finished`); the harm says why it matters.

| Rule | Holds when | Harm if broken |
| --- | --- | --- |
| `CleanEndIsComplete` | The client's track ends cleanly only with every group handed out. | The viewer loses media silently at the end. |
| `CleanEndIsDeclared` | The client's track ends cleanly only after the origin ended it. | A failover reads as the broadcast ending. |
| `NoSkipWhileHeld` | A group the client started fails only once `E` has no route left. | A group is cut short while another path holds the rest. |
| `SettledIsWhole` | A finished read holds every frame of every group, unless `E` has no route left. | A group ends short of its last frame. |
| `Settles` (deadlock) | Every path ends with the read over, or, once the client left, with no relay holding a copy. | Playback hangs, or a relay keeps its upstream subscribed after the viewer left. |

## Run

```sh
quest gate just rs tla moq-net/verify/failover/FailoverChain.tla
quest gate just rs tla moq-net/verify/failover/FailoverChain.tla -config rs/moq-net/verify/failover/Leave.cfg
quest gate just rs tla moq-net/verify/failover/FailoverChain.tla -config rs/moq-net/verify/failover/Deaths.cfg
quest gate just rs tla moq-net/verify/failover/FailoverChain.tla -config rs/moq-net/verify/failover/Mesh.cfg -simulate num=200000 -depth 100
```

The first checks every rule with `E` failing over between `A` and `B`, one death, and one switch per relay, with a client that stays, so a deadlock is a playback hang: about 8.0M states. The second lets the client leave at any point, so a deadlock is a relay still holding a copy after it left; leaving is always possible there, which is why it cannot stand in for the first: about 8.6M states. With one death `E` always has a route, so the third lets both `A` and `B` die: only there does `E`'s front end the track (`FrontFail`), once no dead relay's route still shows (`Withdraw`): about 12.3M states. Each takes three to four minutes and passes at the sync commit. The fourth adds `B`'s route through `A`, so the route also moves at the middle hop, after `B`'s track ended too (`Action::Reroute`). It is too large to exhaust (past 139M distinct states at depth 38 after an hour, its queue still growing), so its `Simulate:` line samples 800k random behaviors in about 7 minutes, and Nightly samples new ones each run. An exhaustive run passed every state through depth 37. Before the reroute, a sample that size missed the depth-31 hang that BFS found after about 48M states, so a passing sample is weak evidence: check a change at the middle hop by running it without the `-simulate` arguments for as long as it takes.

## Reading a failure

Follow Reading a failure in `rs/moq-net/verify/README.md`. Replay the path on the real stack in `rs/moq-net/tests/integration/failover_chain.rs`: relays are `origin::Producer`s linked over the memory transport (`link`, `priced_link`, a cost to make a route win), a relay or hop dies by aborting its sessions, a group read past a failed copy is what makes `Recover` fetch, and time is simulated. Pin a path the code follows as a `residual_*` test with its own config so the others still pass.

## Coverage

Sync commit: `46059266ec9e149c37147577bc40ca3a7cea556a`, plus the splice-stall fix, which holds a track while a dead relay's route still shows, since a session closes its sources before it fails their copies (`Shows`, `Withdraw`), [moq-dev/moq#9276](https://github.com/moq-dev/moq/pull/9276), which splits a switch at one boundary (`Boundary`, `CapAt`), so a reader may join a copy asked from a later group and waits for the rest to be asked and to land, a fetch landing after the end too (`Askable`, `MayLand`), and [moq-dev/moq#9402](https://github.com/moq-dev/moq/pull/9402), which stops the boundary group on a replaced copy where the new one resumes it (`StopAt`). Run `git log 46059266ec.. -- rs/moq-net/src/model/resume.rs rs/moq-net/src/model/front.rs rs/moq-net/src/model/origin.rs rs/moq-net/src/model/track.rs rs/moq-net/src/lite/subscriber.rs` to see what changed since. Follow Coverage in `rs/moq-net/verify/README.md` after a change to a covered function.

| Code | Model |
| --- | --- |
| `rs/moq-net/src/model/resume.rs` `Reader::sync`, `boundary`, `Copy::cap`, `backfill_gaps`, `Producer::subscription`; `rs/moq-net/src/model/track/demand.rs` the folded `cap` | `Boundary`, `CapAt`, `StopAt`, `Stopped`, `Unstopped`, `ResumeAsk`, `Demand`, `Switch`, `Subscribe`, `Frame` |
| `rs/moq-net/src/model/resume.rs` `Reader::resume_at`, `held_at`, `Resumed`; `rs/moq-net/src/model/track.rs` `Consumer::lend_head`, the loan taken in `create_group_at` | `Resume`, `Lend`, `HandOut` |
| `rs/moq-net/src/model/resume.rs` `Reader::take`, `poll_end`; `rs/moq-net/src/model/track.rs` a recovery outliving the track's finish | `HandOut`, `Drained`, `Askable`, `MayLand`, `EndOk` |
| `rs/moq-net/src/model/resume.rs` `Reader::let_go`, `Copy::retains`, `Producer::park` | `LetGo`, `Park` |
| `rs/moq-net/src/model/resume.rs` `Recover::wants`, `poll`, `held`, `poll_serving` | `Recover`, `Holders`, `Exhausted`, `Abort` |
| `rs/moq-net/src/model/resume.rs` `Recover::poll_fetch`, `Fetching`; `rs/moq-net/src/model/track.rs` `fetch_group` | `Ask` |
| `rs/moq-net/src/model/front.rs` `track_ended`, `Action::Finish`; `rs/moq-net/src/model/origin.rs` `TrackIo::end` | `FrontFinish`, `Whole` |
| `rs/moq-net/src/model/front.rs` failover and its unroutable end, `Action::Reroute` | `Switch`, `FrontFail` |
| `rs/moq-net/src/lite/subscriber.rs` `TrackServe::close_if_ending`, `SubscriberDriver::abort` | `Shows`, `Withdraw`, `FrontFail` |
| `rs/moq-net/src/model/resume.rs` `Route::answering`, `Phase::Finished` | `Answering`, `LetGo` |
| `rs/moq-net/src/model/front.rs` `Action::Forget`; `rs/moq-net/src/model/track.rs` `TrackWeak::is_held`, `TrackWeak::poll_held_changed` | `Forget`, `Reads` |

## Not covered

Routing is abstracted to any live upstream winning, so which route wins and announce order are the router's own tests' (`rs/moq-net/src/route/tests`); how a reader picks among its copies, bounds replaced copies, and orders groups is `verify-resume`'s; drops, skips, and re-offers on a hop are `verify-subscribe`'s. The client's track fails only through `FrontFail`, so whether a dead relay whose route still shows can end playback is that step's guard, not a rule. Every hop has an hour's budget, so nothing goes stale or expires. No source refuses the track, so a finished track's reroute never concludes (`Action::Conclude`). Time is abstracted, so the linger may run out at any point once nothing reads the track. The origin writes each group whole, ended at its last frame, so no copy closes a group short with a bare FIN and `Recover::poll`'s `Closed` cause never fires here (`resume_model.rs` and `resume.model.test.ts` own it), and never fails, `E` never dies, a dead relay never returns, and a relay's cache is dropped when it parks. A fetched group holds its head from frame 0 (`lend_head`), and compressed groups are left out. The model lends only at a switch, so the query a front opens on a route before switching to it (`Action::Query`, `follow_demand`) is the unit test `a_query_resuming_a_new_copy_lends_it_the_head`, and a loan never runs out of cache budget. A pass covers two groups of two frames, one death (two in `Deaths.cfg`), one switch per relay, and one client.
