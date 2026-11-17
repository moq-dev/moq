---
name: verify-subscribe
description: Model-check how a lite-07 subscription accounts for each group (`rs/moq-net/src/lite/publisher.rs` `TrackRun` and `GroupServe`, `lite/subscriber.rs` `ServeLoop::apply_response`, `src/tail.rs`) with TLA+. Use when a change touches SUBSCRIBE_DROP, SUBSCRIBE_SKIP, stale (OLD) drops, which groups a publisher serves or names once, Group Stream resets, preemption, and expiry (a lite-07 stream closed when its group expires), a GROUP's Previous Start and the skips it passes over or the publisher holds back for it, or the subscriber's tail, or when a drop bug needs every ordering of the Subscribe Stream against the Group Streams checked.
---

# Verify subscription accounting

`rs/moq-net/verify/subscribe/SubscribeAccounting.tla` models the groups of one answered lite-07 subscription between the publisher's `TrackRun` and the subscriber's `TrackServe`. The draft promises that every sequence in range gets a Group Stream, a SUBSCRIBE_SKIP, or a SUBSCRIBE_DROP, and that every drop is final on its subscription, a stale (OLD) one included: a SUBSCRIBE_UPDATE reopens nothing, so the model leaves updates out. A skip still goes out over a drop of a group never served. Drops and skips travel in order on the Subscribe Stream, each Group Stream travels on its own, and a reset can lose a stream's header. A Group Stream whose group expires stale is closed, not reset: a FIN without GROUP_END, so the subscriber keeps what arrived as a closed group and no drop names it. A stream that never opened is still given up with a drop, and one whose FIN went out is never reset for expiry. A stream reset while serving, evicted or preempted with TOO_FAR_BEHIND for a group that outranks it, is named in a final drop. Each GROUP's Previous Start passes over the skipped run directly below it as a SUBSCRIBE_SKIP would, so the publisher holds such a skip back for the GROUP of a group it still owes, and sends it after all, ahead of the drop or joined to the skip, if it gives that group up instead. A drop of a group whose stream opened goes out after a skip of what its GROUP passed over, since the GROUP may never have arrived. Each model step is one poll of the code that handles one event. The track cursor's verdicts (hand a group out, give it up stale or final, skip it) are free choices, and so are when a stream fails and whether the cache holds the group above a skip, which decides whether the skip is held back. With `Pressure`, so is when the cache pool's debt folds the publisher's record of a group. TLC checks every ordering under the bounds in the config, and reports a deadlock for any state short of quiet where nothing can happen.

## Rules

Each rule is an INVARIANT; the harm says why it matters.

| Rule | Holds when | Harm if broken |
| --- | --- | --- |
| `ServedOnce` | The publisher opens at most one Group Stream for a group. | The group's bytes go out twice, and the subscriber reads two streams of one group. |
| `NamedOnce` | The publisher names a group in at most one drop and at most one skip, and a skip only for a group it never served. The one exception repeats a skip a GROUP passed over, once, ahead of that group's drop. | A second name contradicts a final one: the subscriber accounts the group twice or ends a reader on a group it delivered. |
| `QuietIsSettled` | Once nothing is in flight and the publisher has nothing left to do, the subscriber's tail accounts for every group, a skip held back for a GROUP included. | The subscriber waits out its grace for a group nobody will send, and a relay holds its readers there. |

## Run

```sh
quest gate just rs tla moq-net/verify/subscribe/SubscribeAccounting.tla
```

It checks every rule under `Pressure` over three groups, 1 to 3, so a skip of 1 can be held back for 2's GROUP and a run of 1 and 2 for 3's: about 1.97M distinct states, ten seconds. It passes. Nightly runs every config here and fails if a config with an `\* Open finding:` line passes.

## Reading a failure

Follow Reading a failure in `rs/moq-net/verify/README.md`. Replay the path on the code: the publisher's half with `lite07_run`, `write_group`, `forward`, `drops`, and `SinkSession::with_unacked_fin` in `lite/publisher.rs` tests; the subscriber's half with `lite07_serving`, `apply_response`, and `GroupRecv` in `lite/subscriber.rs` tests.

## Coverage

Sync commit: `3016af623807e939c2e99571a3ec02beb2493579`. Run `git log 3016af6238.. -- rs/moq-net/src/lite/publisher.rs rs/moq-net/src/lite/subscriber.rs rs/moq-net/src/tail.rs rs/moq-net/src/model/track.rs rs/moq-net/src/model/record.rs` to see what changed since. Follow Coverage in `rs/moq-net/verify/README.md` after a change to a covered function.

| Code | Model |
| --- | --- |
| `rs/moq-net/src/lite/publisher.rs` `TrackRun::poll_track` (`Recv::Group`), `serve`, `mark_served`; `rs/moq-net/src/model/record.rs` `Record::told`; `rs/moq-net/src/model/track.rs` `TrackState::previous_start` | `CursorHand`, `Serve`, `SkippedBelow` |
| `rs/moq-net/src/lite/publisher.rs` `TrackRun::give_way`, `shed` | a new group, or one shed, as `Serve`, then `StreamFail` with `old` whose header never arrives |
| `rs/moq-net/src/lite/publisher.rs` `poll_forward_drop`, `forward_range`, `drop_range`, `report`, `hole`; `rs/moq-net/src/model/record.rs` `Record::report`, `tell`, `untold`, `Report::blocks` | `CursorGiveUp`, `dropped` |
| `rs/moq-net/src/lite/publisher.rs` `TrackRun::elides`, `owes_group`, `send_elided` | `held`, the `hold` choice of `CursorGiveUp` |
| `rs/moq-net/src/lite/publisher.rs` `TrackRun::poll_step` (outcomes) | `PubOutcomes`, `nahead` |
| `rs/moq-net/src/lite/publisher.rs` `GroupServe::poll_serve` (`Serve`, `Closed`), `settle_failed`, `poll_stopped`, `closes_expired`, `Ending`; `rs/moq-net/src/transport/boxed.rs` `ListedState::preempted` | `StreamFin`, `StreamFail`, `StreamAck` |
| `rs/moq-net/src/model/record.rs` `Record::fold` | `PubFold`, `folded` |
| `rs/moq-net/src/lite/subscriber.rs` `TrackEntry::open` (with the passed-over skip), `reopen`, `place`; `GroupRecv::poll_serve` | `SubHeader`, `SubFin`, `SubReset` |
| `rs/moq-net/src/lite/subscriber.rs` `ServeLoop::apply_response` (`Drop`, `Skip`) | `OnDrop`, `OnSkip`, `SubRecv` |
| `rs/moq-net/src/tail.rs` `Reading::open`, `Tail::account` | `acct` |
| `rs/moq-net/src/model/track.rs` `Producer::give_up`, `skip_received` | `rec`, `cached` |

What the model asks of that code, each found by TLC as a broken rule without it:

- The publisher serves no sequence its record already told, served or named (`Record::told`), and a skip goes out over a drop alone (`Report::blocks`), so a cursor that hands a group out again after giving it up, as after a route switch, sends nothing (`ServedOnce`, `NamedOnce`).
- A skip held back for a group's GROUP goes out once that group is given up instead: ahead of its drop (`drop_range`, `forward_range`), and joined to its skip, since no GROUP passes over it then (`forward_range`). A skip of the group above that kept the run held would leave it unsent for good (`QuietIsSettled`).
- A drop of a group whose stream opened goes out after a skip of what its GROUP passed over (`GroupOutcomes::dropped`), since a reset can lose the GROUP (`QuietIsSettled`).

## Not covered

The subscription starts answered and covers the groups, so SUBSCRIBE_OK, SUBSCRIBE_STARTED, ranges and Range IDs, fills (`RangeFills`) and the separate request (`Recovery`) a reader wider than a stale drop asks with, floors, SUBSCRIBE_UPDATE, and SUBSCRIBE_FINISH with `drop_unserved` are left out. The tail's grace and the ledger's folding are left out: `QuietIsSettled` asks for the publisher's answer, never the grace. So are priority and order (`Steer`), which stream a preempt picks, the Subscribe Stream's FIN and cancellation, sessions closing, versions before lite-07, relays forwarding drops downstream, the budget a relay records a stale drop under, datagrams, and the copy's readers (`poll_drop`). A route switch appears only as the cursor handing out once more a group it gave up. FETCH is left out: one request and one response, ended by a FIN or a reset, with nothing to race. GROUP_END takeover is left out, and so are frames: how much of a group an expired stream closes on, and GROUP_END's End, which only says where a whole group ends. With one Group Stream per group, the subscriber's wait for a duplicate stream (`Open::Duplicate`) is never reached. A GROUP's Previous Start is modeled as the skipped run directly below it; one naming a known group below, or nothing, passes over nothing, and a run released by an update (`release_elided`) is left out with updates. A pass covers three groups.
