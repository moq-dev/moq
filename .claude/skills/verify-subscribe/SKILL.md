---
name: verify-subscribe
description: Model-check lite-07 subscription group accounting (`rs/moq-net/verify/subscribe/`) with TLA+. Use when changing `SUBSCRIBE_DROP`/`SKIP`, group stream resets, preemption/expiry, or subscriber tail in `rs/moq-net`.
---

# Verify subscription accounting

`rs/moq-net/verify/subscribe/SubscribeAccounting.tla` models the groups of one answered lite-07 subscription between the publisher's `TrackRun` and the subscriber's `TrackServe`. The draft promises that every sequence in range gets a Group Stream, a SUBSCRIBE_SKIP, or a SUBSCRIBE_DROP, and that every drop is final on its subscription, a stale (OLD) one included: a SUBSCRIBE_UPDATE reopens nothing, so the model leaves updates out. A skip still goes out over a drop of a group never served. Drops and skips travel in order on the Subscribe Stream, each Group Stream travels on its own, and a reset can lose a stream's header. A Group Stream whose group expires stale resets with OLD and is named in a stale drop, whether it never opened, is serving, or already sent its FIN and awaits the acknowledgement, so stale bytes never hold the send window; a subscriber that read it whole before the reset keeps the group. A stream reset while serving, evicted or preempted with TOO_FAR_BEHIND for a group that outranks it, is named in a final drop. A group may finish whole at an End, and no other group starts strictly inside its span. Its GROUP_END accounts for those starts, which the publisher then records as told, a drop of the group names the span once it is whole, and the track records the starts inside skipped, which the cursor skips where nothing told them yet. Each model step is one poll of the code that handles one event. The track cursor's verdicts (hand a group out, give it up stale or final, skip it) are free choices, and so are when a stream fails and when a group finishes whole and where it ends. With `Pressure`, so is when the cache pool's debt folds the publisher's record of a group. TLC checks every ordering under the bounds in the config, and reports a deadlock for any state short of quiet where nothing can happen.

## Rules

Each rule is an INVARIANT; the harm says why it matters.

| Rule | Holds when | Harm if broken |
| --- | --- | --- |
| `ServedOnce` | The publisher opens at most one Group Stream for a group. | The group's bytes go out twice, and the subscriber reads two streams of one group. |
| `NamedOnce` | The publisher names a group in at most one drop and a start in at most one skip, and a skip only for a group it never served. A drop may follow the group's own Group Stream, even one sent whole, once it resets. No Group Stream opens for a start inside a whole group's span. | A second name contradicts a final one: the subscriber accounts the group twice or ends a reader on a group it delivered. |
| `QuietIsSettled` | Once nothing is in flight and the publisher has nothing left to do, the subscriber's tail accounts for every start, the ones inside a whole group's span included. | The subscriber waits out its grace for a group nobody will send, and a relay holds its readers there. |

## Run

```sh
quest gate just rs tla moq-net/verify/subscribe/SubscribeAccounting.tla
```

It checks every rule under `Pressure` over three starts, 1 to 3, where group 1 may end at 2, 3, or 4, so its span holds none, one, or both of the starts above it: about 75.7M distinct states, nine minutes. It passes. Nightly runs every config here and fails if a config with an `\* Open finding:` line passes.

## Reading a failure

Follow Reading a failure in `rs/moq-net/verify/README.md`. Replay the path on the code: the publisher's half with `lite07_run`, `write_group`, `forward`, `drops`, and `SinkSession::with_unacked_fin` in `lite/publisher.rs` tests; the subscriber's half with `lite07_serving`, `apply_response`, and `GroupRecv` in `lite/subscriber.rs` tests.

## Coverage

Sync commit: `d791d549a6690b31dd6e6f5173d776fd8ed45573`. Run `git log d791d549a6.. -- rs/moq-net/src/lite/publisher.rs rs/moq-net/src/lite/subscriber.rs rs/moq-net/src/tail.rs rs/moq-net/src/model/track.rs rs/moq-net/src/model/record.rs` to see what changed since. Follow Coverage in `rs/moq-net/verify/README.md` after a change to a covered function.

| Code | Model |
| --- | --- |
| `rs/moq-net/src/lite/publisher.rs` `TrackRun::poll_track` (`Recv::Group`), `serve`, `mark_served`; `rs/moq-net/src/model/record.rs` `Record::told` | `CursorHand` |
| `rs/moq-net/src/model/group.rs` `Producer::finish_at`; `rs/moq-net/src/model/track.rs` `TrackState::appended`, `TrackState::span_end` | `Finish`, `ends`, `CursorHole` |
| `rs/moq-net/src/lite/publisher.rs` `TrackRun::give_way`, `shed` | a new group, or one shed, as `Serve`, then `StreamFail` with `old` whose header never arrives |
| `rs/moq-net/src/lite/publisher.rs` `poll_forward_drop`, `forward_range`, `drop_range`, `report`, `hole`; `rs/moq-net/src/model/record.rs` `Record::report`, `tell`, `untold`, `Report::blocks` | `CursorGiveUp`, `dropped`, `Span` |
| `rs/moq-net/src/lite/publisher.rs` `TrackRun::poll_step` (outcomes, `GroupOutcomes::whole`) | `PubOutcomes` |
| `rs/moq-net/src/lite/publisher.rs` `GroupServe::poll_serve` (`Serve`, `Closed`), `buffer_group_end`, `settle_failed`, `poll_given_up`; `rs/moq-net/src/transport/boxed.rs` `ListedState::preempted` | `StreamFin`, `StreamFail`, `StreamAck` |
| `rs/moq-net/src/model/record.rs` `Record::fold` (`Fold::Debt`, and `Fold::Final`, which folds a subset) | `PubFold`, `folded` |
| `rs/moq-net/src/lite/subscriber.rs` `TrackEntry::open`, `reopen`, `place`; `GroupRecv::poll_serve` (a GROUP_END's span) | `SubHeader`, `SubFin`, `SubReset` |
| `rs/moq-net/src/lite/subscriber.rs` `ServeLoop::apply_response` (`Drop`, `Skip`) | `OnDrop`, `OnSkip`, `SubRecv` |
| `rs/moq-net/src/tail.rs` `Reading::open`, `Reading::account`, `Tail::account` | `acct` |
| `rs/moq-net/src/model/track.rs` `Producer::give_up`, `skip_received` | `rec`, `cached` |

What the model asks of that code, each found by TLC as a broken rule without it:

- The publisher serves no sequence its record already told, served or named (`Record::told`), and a skip goes out over a drop alone (`Report::blocks`), so a cursor that hands a group out again after giving it up, as after a route switch, sends nothing (`ServedOnce`, `NamedOnce`).
- A drop of a group held whole names its span (`forward_range`, `settle_failed`), since a reset can lose its GROUP_END (`QuietIsSettled`).
- A GROUP_END buffered records the starts inside the span as told (`GroupOutcomes::whole`), so the track's end does not name them again (`NamedOnce`).
- The starts inside a span whose group's stream was dropped before it finished whole are skipped where nothing told them (`QuietIsSettled`).
- A stream reset for expiry after its FIN is still named in a drop (`settle_failed` from `GroupState::Closed`), since the reset can discard the header before it is read (`QuietIsSettled`).

## Not covered

The subscription starts answered and covers the groups, so SUBSCRIBE_START, ranges and Range IDs, fills (`RangeFills`) and the separate request (`Recovery`) a reader wider than a stale drop asks with, floors, SUBSCRIBE_UPDATE, and SUBSCRIBE_END with `drop_unserved` are left out, and so are the probe ranges a relay copy asks upstream for a time start it cannot prove (`Kind::Probe`), which an answer's SUBSCRIBE_DROP and SUBSCRIBE_START treat as a fill. A group a relay fetches the head of before serving (`fetches_from`) is the free choices already here: its stream opens late, or it is named in a drop once the fetch is refused. A copy's cache sheds frames under the pool and a session's live-edge budget (`shed_edge`), which `Evict` and a failing stream stand for. The tail's grace and the ledger's folding are left out: `QuietIsSettled` asks for the publisher's answer, never the grace. So are priority and order (`Steer`), which stream a preempt picks, the Subscribe Stream's FIN and cancellation, sessions closing, versions before lite-07, relays forwarding drops downstream, the budget a relay records a stale drop under, datagrams, and the copy's readers (`poll_drop`). A route switch appears only as the cursor handing out once more a group it gave up. FETCH is left out: one request and one response, ended by a FIN or a reset, with nothing to race. GROUP_END takeover is left out, and so are frames: an End is at most one past the highest start, so a span may run past the range, and its delta coding is left to the codec tests. With one Group Stream per group, the subscriber's wait for a duplicate stream (`Open::Duplicate`) is never reached. A pass covers three starts.
