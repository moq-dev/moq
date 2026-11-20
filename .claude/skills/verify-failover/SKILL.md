---
name: verify-failover
description: Model-check failover along a chain of relays (`rs/moq-net/verify/failover/FailoverChain.tla`) with TLA+. Use when a change touches how a relay's front switches routes or ends a track (`model/front.rs` `track_ended`, `model/origin.rs` `Action::Finish`, `park`), how a switch asks the new copy (`model/resume.rs` `Reader::sync`, `backfill_gaps`, `Producer::subscription`), how a group continues on another copy mid-read (`Recover`, `Fetching`), or when a failover bug spans more than one hop and needs every ordering of deaths, switches, and frames checked rather than a random sweep.
---

# Verify failover along a relay chain

`rs/moq-net/verify/failover/FailoverChain.tla` models one track served to a client `C` through relay `E`, which starts on relay `A` and may fail over to relay `B`; both pull from the origin `O`, and with `Mesh` `B` may also route through `A`. Each layer is abstracted to the steps its own model checked, so the state stays small: a route change is one `Switch`, a switch asks the new copy from where the readers got to, past the frames of the newest group its relay holds, which it lends the new copy, and each reader hands a group out once and ends once nothing it replaced may still deliver (`verify-resume`), and a group handed out crosses a hop unless its session dies (`verify-subscribe`, with an hour's budget). What the chain adds is frames: a group whose copy fails or stalls mid-read continues on another copy at the same frame (`Recover`), from a copy its relay holds, from the serving copy, or by fetching the rest from the serving route, which asks its own upstream in turn. A relay whose track ended keeps moving the reads that still hold it (`Switch` after `FrontFinish`) and forgets the track only once nothing reads it (`Forget`, the linger).

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

The first checks every rule with `E` failing over between `A` and `B`, one death, and one switch per relay, with a client that stays, so a deadlock is a playback hang: about 3.8M states. The second lets the client leave at any point, so a deadlock is a relay still holding a copy after it left; leaving is always possible there, which is why it cannot stand in for the first: about 4.1M states. Each takes about three minutes and passes at the sync commit. The third adds `B`'s route through `A`, so the route also moves at the middle hop, after `B`'s track ended too (`Action::Reroute`). It is too large to exhaust (past 139M distinct states at depth 38 after an hour, its queue still growing), so its `Simulate:` line samples 800k random behaviors in about 7 minutes, and Nightly samples new ones each run. An exhaustive run passed every state through depth 37. Before the reroute, a sample that size missed the depth-31 hang that BFS found after about 48M states, so a passing sample is weak evidence: check a change at the middle hop by running it without the `-simulate` arguments for as long as it takes.

## Reading a failure

Follow Reading a failure in `rs/moq-net/verify/README.md`. Replay the path on the real stack in `rs/moq-net/tests/integration/failover_chain.rs`: relays are `origin::Producer`s linked over the memory transport (`link`, `priced_link`, a cost to make a route win), a relay or hop dies by aborting its sessions, a group read past a failed copy is what makes `Recover` fetch, and time is simulated. Pin a path the code follows as a `residual_*` test with its own config so the others still pass.

## Coverage

Sync commit: `61f89568394638553a519d7d8cfc0019385dd3a0`, plus [moq-dev/moq#9048](https://github.com/moq-dev/moq/pull/9048), which mirrored the code since 88572c583e and kept a finished track a read holds past its linger, and [moq-dev/moq#9155](https://github.com/moq-dev/moq/pull/9155), which resumes the newest group past the frames held and lends them to the new copy (`Resume`, `Lend`). Run `git log 61f8956839.. -- rs/moq-net/src/model/resume.rs rs/moq-net/src/model/front.rs rs/moq-net/src/model/origin.rs rs/moq-net/src/model/track.rs` to see what changed since. Follow Coverage in `rs/moq-net/verify/README.md` after a change to a covered function.

| Code | Model |
| --- | --- |
| `rs/moq-net/src/model/resume.rs` `Reader::sync`, `backfill_gaps`, `Producer::subscription` | `ResumeAsk`, `Demand`, `Switch`, `Subscribe` |
| `rs/moq-net/src/model/resume.rs` `Reader::resume_at`, `held_at`, `Resumed`; `rs/moq-net/src/model/track.rs` `Consumer::lend_head`, the loan taken in `create_group_at` | `Resume`, `Lend`, `HandOut` |
| `rs/moq-net/src/model/resume.rs` `Reader::take`, `poll_end` | `HandOut`, `Drained`, `EndOk` |
| `rs/moq-net/src/model/resume.rs` `Reader::let_go`, `Copy::retains`, `Producer::park` | `LetGo`, `Park` |
| `rs/moq-net/src/model/resume.rs` `Recover::wants`, `poll`, `held`, `poll_serving` | `Recover`, `Holders`, `Exhausted`, `Abort` |
| `rs/moq-net/src/model/resume.rs` `Recover::poll_fetch`, `Fetching`; `rs/moq-net/src/model/track.rs` `fetch_group` | `Ask` |
| `rs/moq-net/src/model/front.rs` `track_ended`, `Action::Finish`; `rs/moq-net/src/model/origin.rs` `TrackIo::end` | `FrontFinish`, `Whole` |
| `rs/moq-net/src/model/front.rs` failover and its unroutable end, `Action::Reroute` | `Switch`, `FrontFail` |
| `rs/moq-net/src/model/resume.rs` `Route::answering`, `Phase::Finished` | `Answering`, `LetGo` |
| `rs/moq-net/src/model/front.rs` `Action::Forget`; `rs/moq-net/src/model/track.rs` `TrackWeak::is_held`, `TrackWeak::poll_held_changed` | `Forget`, `Reads` |

## Not covered

Routing is abstracted to any live upstream winning, so which route wins and announce order are the router's own tests' (`rs/moq-net/src/route/tests`); how a reader picks among its copies, bounds replaced copies, and orders groups is `verify-resume`'s; drops, skips, and re-offers on a hop are `verify-subscribe`'s. The client's track fails only through `FrontFail`, so whether one dead relay can end playback is that step's guard, not a rule. Every hop has an hour's budget, so nothing goes stale or expires. No source refuses the track, so a finished track's reroute never concludes (`Action::Conclude`). Time is abstracted, so the linger may run out at any point once nothing reads the track. The origin writes each group whole and never fails, `E` never dies, a dead relay never returns, and a relay's cache is dropped when it parks. A fetched group holds its head from frame 0 (`lend_head`), and compressed groups are left out. The model lends only at a switch, so the query a front opens on a route before switching to it (`Action::Query`, `follow_demand`) is the unit test `a_query_resuming_a_new_copy_lends_it_the_head`, and a loan never runs out of cache budget. A pass covers two groups of two frames, one death, one switch per relay, and one client.
