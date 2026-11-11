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
just rs tla moq-net/verify/failover/FailoverChain.tla
just rs tla moq-net/verify/failover/FailoverChain.tla -config rs/moq-net/verify/failover/Leave.cfg
just rs tla moq-net/verify/failover/FailoverChain.tla -config rs/moq-net/verify/failover/EndedRelay.cfg
```

The first checks every rule with `E` failing over between `A` and `B`, one death, and one switch per relay, with a client that stays, so a deadlock is a playback hang: about 2.0M states. The second lets the client leave at any point, so a deadlock is a relay still holding a copy after it left; leaving is always possible there, which is why it cannot stand in for the first: about 2.2M states. Each takes under 30 seconds on 24 cores and passes at the sync commit. The third adds `B`'s route through `A`, so the route also moves at the middle hop: it fails at the sync commit on the open finding below at depth 31, after about 50M distinct states and 11 minutes on 8 workers.

## Open finding: an ended relay cannot move a fetch off a dying route

`EndedRelay.cfg` breaks `NoSkipWhileHeld`. `E` reads group 0 through `A` and hands out group 1, then switches to `B`, asking it from group 1, and fetches the rest of group 0 from `B`. `B` moves its route to `A`, its copy from group 1 completes, and its track ends (`Action::Finish`); then `A` dies. `B`'s front no longer switches, so its fetch for group 0 fails although `B` still reaches `O` directly (`Fetching` returns the dead copy's error once `route.end()` is set), and `E`, whose serving route gave up on the group, cuts it short. The real stack breaks the same rule earlier, on the first code finding below, so this path has no replay of its own yet; once that is fixed, replay it in `failover_chain.rs` and pin or fix it. Once both are fixed, merge `EndedRelay.cfg`'s `Mesh` into `FailoverChain.cfg` if the bounds allow, and delete `EndedRelay.cfg`.

## Open findings: the code falls short of the model

The model lets a fetch reach the serving route whenever that route lives, after the track ended too. Replaying its paths in `rs/moq-net/tests/integration/failover_chain.rs` found two places where the code does not, each pinned as a `residual_*` test that passes while the bug stands; `a_half_read_group_continues_through_the_new_route` is the same path without the end, which passes. Once one is fixed, flip its test.

- `residual_ending_the_track_fails_a_fetch_in_flight`: `E`'s fetch of the rest of a group is in flight through `B` when the publisher ends the track. `B`'s copy, asked from a later group, completes, `B`'s fetch from the publisher is cancelled, and `E`'s read fails with `NotFound` while the publisher still writes the rest (`NoSkipWhileHeld`).
- `residual_a_fetch_from_a_completed_copy_never_leaves_the_relay`: the same group read on only after `E`'s serving copy completed. `Recover::poll_fetch` calls `fetch_group` on that copy, but no FETCH goes on the wire, so the read waits out the subscriber's budget (`Settles`).

## Reading a failure

A counterexample is a path through the model, not yet a bug. Replay it on the real stack in `rs/moq-net/tests/integration/failover_chain.rs`: relays are `origin::Producer`s linked over the memory transport (`link`, `priced_link`, a cost to make a route win), a relay or hop dies by aborting its sessions, a group read past a failed copy is what makes `Recover` fetch, and time is simulated. If the code does not follow the path, the model is wrong: fix the model to match the code. If it does, report the path and the rule in the code's words, and pin it as a `residual_*` test with its own config so the others still pass. Do not weaken a rule, add a constraint, or shrink the bounds until the check passes.

## Coverage

Sync commit: `88572c583e2f64c7f5fc9bfa59df90706308497e`. Run `git log 88572c583e.. -- rs/moq-net/src/model/resume.rs rs/moq-net/src/model/front.rs rs/moq-net/src/model/origin.rs rs/moq-net/src/model/track.rs` to see what changed since. After a change to a covered function, mirror it in the model, rerun every config, and move the sync commit forward only once the model describes the code at the new commit.

| Code | Model |
| --- | --- |
| `rs/moq-net/src/model/resume.rs` `Reader::sync`, `backfill_gaps`, `Producer::subscription` | `ResumeAsk`, `Demand`, `Switch`, `Subscribe` |
| `rs/moq-net/src/model/resume.rs` `Reader::take`, `poll_end` | `HandOut`, `Drained`, `EndOk` |
| `rs/moq-net/src/model/resume.rs` `Reader::let_go`, `Copy::retains`, `Producer::park` | `LetGo`, `Park` |
| `rs/moq-net/src/model/resume.rs` `Recover::wants`, `poll`, `held`, `poll_serving` | `Recover`, `Holders`, `Exhausted`, `Abort` |
| `rs/moq-net/src/model/resume.rs` `Recover::poll_fetch`, `Fetching`; `rs/moq-net/src/model/track.rs` `fetch_group` | `Ask` |
| `rs/moq-net/src/model/front.rs` `track_ended`, `Action::Finish`; `rs/moq-net/src/model/origin.rs` `TrackIo::end` | `FrontFinish`, `Whole` |
| `rs/moq-net/src/model/front.rs` failover and its unroutable end | `Switch`, `FrontFail` |

## Not covered

Routing is abstracted to any live upstream winning, so feasibility, ROUTE_END, and announce order are `verify-route`'s; how a reader picks among its copies, bounds replaced copies, and orders groups is `verify-resume`'s; drops, skips, and re-offers on a hop are `verify-subscribe`'s. The client's track fails only through `FrontFail`, so whether one dead relay can end playback is that step's guard, not a rule. Every hop has an hour's budget, so nothing goes stale or expires. The origin writes each group whole and never fails, `E` never dies, a dead relay never returns, and a relay's cache is dropped when it parks. A fetched group holds its head from frame 0 (`lend_head`), and compressed groups are left out. A pass covers two groups of two frames, one death, one switch per relay, and one client.
