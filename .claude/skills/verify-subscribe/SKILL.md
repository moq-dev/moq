---
name: verify-subscribe
description: Model-check how a lite-07 subscription accounts for each group (`rs/moq-net/src/lite/publisher.rs` `TrackRun` and `GroupServe`, `lite/subscriber.rs` `ServeLoop::apply_response`, `src/tail.rs`) with TLA+. Use when a change touches SUBSCRIBE_DROP, SUBSCRIBE_SKIP, stale (OLD) drops, which groups a publisher serves or names once, Group Stream resets and expiry (a lite-07 stream closed when its group expires), a GROUP's Previous Start and the skips it passes over or the publisher holds back for it, YIELDED resets and continuations of a paused group, duplicate Group Streams, or the subscriber's tail, or when a drop bug needs every ordering of the Subscribe Stream against the Group Streams checked.
---

# Verify subscription accounting

`rs/moq-net/verify/subscribe/SubscribeAccounting.tla` models the groups of one answered lite-07 subscription between the publisher's `TrackRun` and the subscriber's `TrackServe`. The draft promises that every sequence in range gets a Group Stream, a SUBSCRIBE_SKIP, or a SUBSCRIBE_DROP, and that every drop is final on its subscription, a stale (OLD) one included: a SUBSCRIBE_UPDATE reopens nothing, so the model leaves updates out. A skip still goes out over a drop of a group never served. Drops and skips travel in order on the Subscribe Stream, each Group Stream travels on its own, and a reset can lose a stream's header. A Group Stream whose group expires stale is closed, not reset: a FIN without GROUP_END, so the subscriber keeps what arrived as a closed group and no drop names it. A stream that never opened is still given up with a drop, and one whose FIN went out is never reset for expiry. Each GROUP's Previous Start passes over the skipped run directly below it as a SUBSCRIBE_SKIP would, so the publisher holds such a skip back for the GROUP of a group it still owes, and sends it after all, ahead of the drop or joined to the skip, if it gives that group up instead. A drop of a group whose stream opened goes out after a skip of what its GROUP passed over, since the GROUP may never have arrived. A publisher out of stream credit may reset a Group Stream with YIELDED, which pauses the group instead of accounting for it: the subscriber keeps the frames it read, and the publisher owes a continuation, a Group Stream that starts at the reset stream's own Frame Start, or a drop. A continuation may overtake the YIELDED reset, or even the header, of the stream it continues, and is the only second Group Stream of a group. Each model step is one poll of the code that handles one event. The track cursor's verdicts (hand a group out, give it up stale or final, skip it) are free choices, and so are when the publisher yields a stream, whether it continues a paused group or gives it up, and whether the cache holds the group above a skip, which decides whether the skip is held back. With `Pressure`, so is when the cache pool's debt folds the publisher's record of a group. TLC checks every ordering under the bounds in the config.

## Rules

Each rule is an INVARIANT; the harm says why it matters.

| Rule | Holds when | Harm if broken |
| --- | --- | --- |
| `ServedOnce` | The publisher opens a second Group Stream for a group only to continue the group's latest stream, which it reset with YIELDED. | The group's bytes go out twice, or a continuation resumes a group nobody paused. |
| `NamedOnce` | The publisher names a group in at most one drop and at most one skip, and a skip only for a group it never served. The one exception repeats a skip a GROUP passed over, once, ahead of that group's drop. | A second name contradicts a final one: the subscriber accounts the group twice or ends a reader on a group it delivered. |
| `QuietIsSettled` | Once nothing is in flight and the publisher has nothing left to do, the subscriber's tail accounts for every group, a skip held back for a GROUP included. | The subscriber waits out its grace for a group nobody will send, and a relay holds its readers there. |
| `PausedIsOwed` | While the publisher owes a paused group, the subscriber's paused copy stays owed, unless a drop or skip, a stream read to its FIN, or a reset of another code that arrived with its header accounts for it. | The tail settles on a YIELDED reset alone and the subscription ends before the continuation arrives. |
| `ContinuesWithinHeld` | A continuation that takes over a paused copy starts no later than the frames the copy holds (`Continuation.cfg`). | The subscriber treats the continuation as malformed, or a frame a reset discarded is never delivered. |
| `OneReader` | The subscriber reads at most one Group Stream of a group at a time; a continuation waits, unread, for the stream it continues to end. | Two streams write one group's frames at once. |

## Run

```sh
quest gate just rs tla moq-net/verify/subscribe/SubscribeAccounting.tla
quest gate just rs tla moq-net/verify/subscribe/SubscribeAccounting.tla -config rs/moq-net/verify/subscribe/Continuation.cfg
```

The first checks every rule but `ContinuesWithinHeld` under `Pressure`, over two groups, 1 and 2 so a skip of 1 can be held back for 2's GROUP, six Group Streams, and two YIELDED resets per group, with frames left out: about 15.9M distinct states, four minutes. The second gives one group two frames and three YIELDED resets, without `Pressure`, so it also checks where each continuation starts and where an expired stream closes: about 493K distinct states, five seconds. Both pass. Nightly runs every config here and fails if a config with an `\* Open finding:` line passes.

## Reading a failure

Follow Reading a failure in `rs/moq-net/verify/README.md`. Replay the path on the code: the publisher's half with `lite07_run`, `write_group`, `forward`, `drops`, and `SinkSession::with_unacked_fin` in `lite/publisher.rs` tests; the subscriber's half with `lite07_serving`, `apply_response`, and `GroupRecv` in `lite/subscriber.rs` tests.

## Coverage

Sync commit: `157042024b139ef0153e1ab71ea40dfdf5e08993`. Run `git log 157042024b.. -- rs/moq-net/src/lite/publisher.rs rs/moq-net/src/lite/subscriber.rs rs/moq-net/src/tail.rs rs/moq-net/src/model/track.rs rs/moq-net/src/model/record.rs` to see what changed since. Follow Coverage in `rs/moq-net/verify/README.md` after a change to a covered function.

| Code | Model |
| --- | --- |
| `rs/moq-net/src/lite/publisher.rs` `TrackRun::poll_track` (`Recv::Group`), `serve`, `mark_served`; `rs/moq-net/src/model/record.rs` `Record::told`; `rs/moq-net/src/model/track.rs` `TrackState::previous_start` | `CursorHand`, `Serve`, `SkippedBelow` |
| `rs/moq-net/src/lite/publisher.rs` `TrackRun::give_way`, `shed` | a new group, or one shed, as `Serve`, then `StreamFail` with `old` whose header never arrives; a continuation as `StreamYield` of a stream whose header never arrives |
| `rs/moq-net/src/lite/publisher.rs` `poll_forward_drop`, `forward_range`, `drop_range`, `report`, `hole`; `rs/moq-net/src/model/record.rs` `Record::report`, `tell`, `untold`, `Report::blocks` | `CursorGiveUp`, `dropped` |
| `rs/moq-net/src/lite/publisher.rs` `TrackRun::elides`, `owes_group`, `send_elided` | `held`, the `hold` choice of `CursorGiveUp` |
| `rs/moq-net/src/lite/publisher.rs` `TrackRun::poll_step` (outcomes) | `PubOutcomes`, `nahead` |
| `rs/moq-net/src/lite/publisher.rs` `GroupServe::poll_serve` (`Serve`, `Closed`), `settle_failed`, `poll_stopped`, `closes_expired`, `Ending` | `StreamFin`, `StreamClose`, `StreamFail`, `StreamAck` |
| `rs/moq-net/src/model/record.rs` `Record::fold` | `PubFold`, `folded` |
| `rs/moq-net/src/lite/subscriber.rs` `TrackEntry::open` (with the passed-over skip), `reopen`, `place`; `GroupRecv::poll_serve` (the duplicate wait) | `SubHeader`, `SubFin`, `SubReset`, `SubResetHeld` |
| `rs/moq-net/src/lite/subscriber.rs` `ServeLoop::apply_response` (`Drop`, `Skip`) | `OnDrop`, `OnSkip`, `SubRecv` |
| `rs/moq-net/src/tail.rs` `Reading::open`, `Tail::account`, `accounts` | `acct`, `reading` |
| `rs/moq-net/src/model/track.rs` `Producer::give_up`, `skip_received` | `rec`, `cached` |
| `rs/moq-net/src/lite/publisher.rs` `GroupServe::settle_failed`, `TrackRun::open`; `rs/moq-net/src/transport/boxed.rs` `SendStream::preempted` | `StreamYield`, `PubContinue`, `PausedGiveUp` |
| `rs/moq-net/src/lite/subscriber.rs` `TrackEntry::pause`, `resume`, `dropped`, `read_yielding`, `end_yielding`, `YieldVerdict`, `GroupRecv::poll_serve`, `FrameIngest::dispatch` | the YIELDED branch of `SubReset`, `SubFrame`, `reading`, `sdrop`, `paused` |

What the model asks of that code, each found by TLC as a broken rule without it:

- A YIELDED reset takes back only what its own stream's header added to the ledger (`reading`): `TrackEntry::open` reads whether the tail already accounted the group before the stream accounts it (`Yielding::owed`). A header that arrives once the ledger settled the group adds nothing, so a YIELDED stream whose header arrives after its continuation finished the group owes nothing again (`QuietIsSettled`).
- A continuation that takes over from a held position inherits that account (`TrackEntry::resume`), so its own YIELDED reset takes it back.
- A drop that reached a stream still being read ends its group once the YIELDED reset arrives (`TrackEntry::dropped`). The model leaves the copy paused and accounted; the code aborts the group with the drop's error, which the drop already accounted, so the tail settles either way.
- A stream waits, held, while another stream of its group is still read, even one a final drop aborted, not only while the copy is open (`OneReader`).
- The publisher serves no sequence its record already told, served or named (`Record::told`), and a skip goes out over a drop alone (`Report::blocks`), so a cursor that hands a group out again after giving it up, as after a route switch, sends nothing (`ServedOnce`, `NamedOnce`).
- A skip held back for a group's GROUP goes out once that group is given up instead: ahead of its drop (`drop_range`, `forward_range`), and joined to its skip, since no GROUP passes over it then (`forward_range`). A skip of the group above that kept the run held would leave it unsent for good (`QuietIsSettled`).
- A drop of a group whose stream opened, or that waited paused, goes out after a skip of what its GROUP passed over (`GroupOutcomes::dropped`), since a reset can lose the GROUP (`QuietIsSettled`).

## Not covered

The subscription starts answered and covers the groups, so SUBSCRIBE_OK, SUBSCRIBE_STARTED, ranges and Range IDs, fills (`RangeFills`) and the separate request (`Recovery`) a reader wider than a stale drop asks with, floors, SUBSCRIBE_UPDATE, and SUBSCRIBE_FINISH with `drop_unserved` are left out. The tail's grace and the ledger's folding are left out: `QuietIsSettled` asks for the publisher's answer, never the grace. So are priority and order (`Steer`), the Subscribe Stream's FIN and cancellation, sessions closing, versions before lite-07, relays forwarding drops downstream, the budget a relay records a stale drop under, datagrams, and the copy's readers (`poll_drop`). A route switch appears only as the cursor handing out once more a group it gave up. FETCH is left out: one request and one response, ended by a FIN or a reset, with nothing to race. GROUP_END takeover is left out: only the YIELDED reset pauses a group, and the publisher yields only a stream still serving. So is how much of a group an expired stream closes on, beyond `Continuation.cfg`'s two frames, and GROUP_END's End, which only says where a whole group ends. A GROUP's Previous Start is modeled as the skipped run directly below it; one naming a known group below, or nothing, passes over nothing, and a run released by an update (`release_elided`) is left out with updates. Runs are as long as the two groups allow, so a held run never grows past one group. A pass covers two groups, six Group Streams, and two YIELDED resets per group, or one group of two frames with three YIELDED resets in `Continuation.cfg`.
