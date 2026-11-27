---
name: verify-subscribe
description: Model-check lite-07 subscription group accounting (`rs/moq-net/verify/subscribe/`) with TLA+. Use when changing `SUBSCRIBE_DROP`/`SKIP`, `GROUP_END`, group stream resets, preemption/expiry, or subscriber tail in `rs/moq-net`.
---

# Verify subscription accounting

`rs/moq-net/verify/subscribe/SubscribeAccounting.tla` models the groups of one answered lite-07 subscription between the publisher's `TrackRun` and the subscriber's `TrackServe`. The draft promises that every start in range gets a Group Stream, is accounted for by a GROUP_END, or is named in a SUBSCRIBE_DROP (with SKIP for declared skips, or an error code), and that every drop is final on its subscription, a stale (OLD) one included: a SUBSCRIBE_UPDATE reopens nothing, so the model leaves updates out. No skip goes out over a drop, but for the starts of a GROUP_END a reset may have cut. Drops and skips travel in order on the Subscribe Stream, and each Group Stream travels on its own. A Group Stream whose group expires stale resets with OLD, whether it never opened, is serving, or already sent its FIN and awaits the acknowledgement, so stale bytes never hold the send window; a subscriber that read it whole before the reset keeps the group. A stream reset while serving, evicted or preempted with TOO_FAR_BEHIND for a group that outranks it, is final. Under `ReliableReset` (a reset delivers its prefix: raw QUIC or QMux with RESET_STREAM_AT, never WebTransport), once the transport took a stream's GROUP header, a reset still delivers it, which accounts for the group, so no drop names it. Any other reset is named in a drop: before then the header never left, and a plain RESET_STREAM lets the peer discard it unread, or still deliver it beside the drop. The subscriber may stop a stream whose header it read, cutting what follows; a stop names the group in a drop only when it is EVICTED and beat the header's acknowledgement. A group may finish whole at an End, and no other group starts strictly inside its span. Its GROUP_END accounts for those starts, which the publisher then records as told, a drop of the group names the span once it is whole, and the track records the starts inside skipped, which the cursor skips where nothing told them yet. A reset or stop that still accounts for the group may cut its GROUP_END, so the publisher skips the starts it told again. Each model step is one poll of the code that handles one event. The track cursor's verdicts (hand a group out, give it up stale or final, skip it) are free choices, and so are when a stream fails and when a group finishes whole and where it ends. With `Pressure`, so is when the cache pool's debt folds the publisher's record of a group. TLC checks every ordering under the bounds in the config, and reports a deadlock for any state short of quiet where nothing can happen.

## Rules

Each rule is an INVARIANT; the harm says why it matters.

| Rule | Holds when | Harm if broken |
| --- | --- | --- |
| `ServedOnce` | The publisher opens at most one Group Stream for a group. | The group's bytes go out twice, and the subscriber reads two streams of one group. |
| `NamedOnce` | The publisher names a group in at most one drop and a start in at most one skip, and skips no start it served or dropped, besides the starts a reset cut out of a GROUP_END. A drop may follow the group's own Group Stream, even one sent whole, once it resets. | A second name contradicts a final one: the subscriber accounts the group twice or ends a reader on a group it delivered. |
| `NoContradictorySkip` | No start a skip named has a Group Stream, and none inside a whole group's span does. | The subscriber records a start skipped while its group arrives, and a reader misses it. |
| `QuietIsSettled` | Once nothing is in flight and the publisher has nothing left to do, the subscriber's tail accounts for every start, the ones inside a span whose GROUP_END a reset cut included. lite-07 has no grace, so this is the only way a start settles. | The subscriber waits forever for a group nobody will send, and a relay holds its readers there. |

## Run

```sh
quest gate just rs tla moq-net/verify/subscribe/SubscribeAccounting.tla
```

It checks every rule under `Pressure` and `ReliableReset` over three starts, 1 to 3, where group 1 may end at 2, 3, or 4, so its span holds none, one, or both of the starts above it: about 78.7M distinct states, 11 minutes. It passes. Nightly runs every config here and fails if a config with an `\* Open finding:` line passes; none has one. `CutGroupEnd.cfg` checks a stream reset after its FIN, which delivers only its header and cuts the GROUP_END, so the publisher skips its starts again, over two starts without `Pressure` in a second:

```sh
quest gate just rs tla moq-net/verify/subscribe/SubscribeAccounting.tla -config rs/moq-net/verify/subscribe/CutGroupEnd.cfg
```

`PlainReset.cfg` checks a peer without RESET_STREAM_AT, or any WebTransport session (`ReliableReset = FALSE`), where every reset group is named in a drop that may land beside a header the subscriber read, over three starts without `Pressure`: about 32.2M distinct states, 4 minutes. It passes.

```sh
quest gate just rs tla moq-net/verify/subscribe/SubscribeAccounting.tla -config rs/moq-net/verify/subscribe/PlainReset.cfg
```

## Reading a failure

Follow Reading a failure in `rs/moq-net/verify/README.md`. Replay the path on the code: the publisher's half with `lite07_run`, `write_group`, `forward`, `drops`, and `SinkSession::with_unacked_fin` in `lite/publisher.rs` tests; the subscriber's half with `lite07_serving`, `apply_response`, and `GroupRecv` in `lite/subscriber.rs` tests.

## Coverage

Sync commit: `f3315bb478a55cb4514a0d114858254fb4f1c039`. Run `git log f3315bb478.. -- rs/moq-net/src/lite/publisher.rs rs/moq-net/src/lite/subscriber.rs rs/moq-net/src/tail.rs rs/moq-net/src/model/track.rs rs/moq-net/src/model/record.rs` to see what changed since. Follow Coverage in `rs/moq-net/verify/README.md` after a change to a covered function.

| Code | Model |
| --- | --- |
| `rs/moq-net/src/lite/publisher.rs` `TrackRun::poll_track` (`Recv::Group`), `serve`, `mark_served`; `rs/moq-net/src/model/record.rs` `Record::told` | `CursorHand` |
| `rs/moq-net/src/model/group.rs` `Producer::finish_at`; `rs/moq-net/src/model/track.rs` `TrackState::appended`, `TrackState::span_end` | `Finish`, `ends`, `CursorHole` |
| `rs/moq-net/src/lite/publisher.rs` `TrackRun::give_way`, `shed` | a new group, or one shed, as `Serve`, then `StreamFail` with `old` whose header never arrives |
| `rs/moq-net/src/lite/publisher.rs` `poll_forward_drop`, `forward_range`, `drop_range`, `report`, `hole`; `rs/moq-net/src/model/record.rs` `Record::report`, `tell`, `untold` | `CursorGiveUp`, `Span` |
| `rs/moq-net/src/lite/publisher.rs` `TrackRun::poll_step` (outcomes, `GroupOutcomes::whole`, `GroupOutcomes::cut`) | `PubOutcomes`, `CutSkip` |
| `rs/moq-net/src/lite/publisher.rs` `GroupServe::poll_serve` (`Serve`, `Closed`), `buffer_group_end`, `settle_failed`, `settle_reset`, `accounts`, `poll_given_up`, `GroupContext::reliable_reset`; `rs/moq-net/src/transport/boxed.rs` `Held::reset`, `ListedState::preempted` | `StreamHold`, `StreamFin`, `StreamFail`, `StreamAck`, `SubStop`, `Kept` |
| `rs/moq-net/src/model/record.rs` `Record::fold` | `PubFold`, `folded` |
| `rs/moq-net/src/lite/subscriber.rs` `TrackEntry::open`, `reopen`, `place`; `GroupRecv::poll_serve` (a GROUP_END's span), `UniServe::abort` | `SubHeader`, `SubFin`, `SubReset`, `SubStop` |
| `rs/moq-net/src/lite/subscriber.rs` `ServeLoop::apply_response` (`Drop`, with `StreamError::Skip` as skip) | `OnDrop`, `OnSkip`, `SubRecv` |
| `rs/moq-net/src/tail.rs` `Reading::open`, `Reading::account`, `Tail::account`, `Tail::proven` | `acct` |
| `rs/moq-net/src/model/track.rs` `Producer::give_up`, `skip_received` | `rec`, `cached` |

What the model asks of that code, each found by TLC as a broken rule without it:

- The publisher serves no sequence its record already told, served or named (`Record::told`), so a cursor that hands a group out again after giving it up, as after a route switch, sends nothing (`ServedOnce`, `NamedOnce`).
- A drop of a group held whole names its span (`forward_range`, `settle_failed`), since no GROUP_END went out (`QuietIsSettled`).
- A GROUP_END buffered records the starts inside the span as told (`GroupOutcomes::whole`), so the track's end does not name them again (`NamedOnce`).
- The starts inside a span whose group's stream was dropped before it finished whole are skipped where nothing told them (`QuietIsSettled`).
- A reset or stop after a GROUP_END that still accounts for the group skips the starts the GROUP_END told again (`GroupOutcomes::cut`), since it may cut the GROUP_END on the wire (`QuietIsSettled`, `CutGroupEnd.cfg`). Those starts may include one dropped before the group finished, which the skip changes nothing for, so `NamedOnce` exempts them.
- A stream reset before the transport took its header is named in a drop (`accounts`), since the reset loses the header (`QuietIsSettled`).
- A stream reset without RESET_STREAM_AT, or over WebTransport, is named in a drop even after the transport took its header, since the peer may discard it unread (`GroupContext::reliable_reset`, `QuietIsSettled`, `PlainReset.cfg`).

## Not covered

The subscription starts answered and covers the groups, so SUBSCRIBE_START, ranges and Range IDs, fills (`RangeFills`) and the separate request (`Recovery`) a reader wider than a stale drop asks with, floors, SUBSCRIBE_UPDATE, and SUBSCRIBE_END with `drop_unserved` are left out, and so are the probe ranges a relay copy asks upstream for a time start it cannot prove (`Kind::Probe`), which an answer's SUBSCRIBE_DROP and SUBSCRIBE_START treat as a fill. A group a relay fetches the head of before serving (`fetches_from`) is the free choices already here: its stream opens late, or it is named in a drop once the fetch is refused. A copy's cache sheds frames under the pool and a session's live-edge budget (`shed_edge`), which `Evict` and a failing stream stand for. The ledger's folding is left out, and lite-07 has no grace: `QuietIsSettled` asks for the publisher's answer. So are priority and order (`Steer`), which stream a preempt picks, the Subscribe Stream's FIN and cancellation, sessions closing, versions before lite-07, relays forwarding drops downstream, the budget a relay records a stale drop under, datagrams, and the copy's readers (`poll_drop`). A route switch appears only as the cursor handing out once more a group it gave up. FETCH is left out: one request and one response, ended by a FIN or a reset, with nothing to race. GROUP_END takeover is left out, and so are subscriber-side drain of an already-buffered GROUP_END on local abort (`FrameIngest::try_drain_end`) and frames: an End is at most one past the highest start, so a span may run past the range, and its delta coding is left to the codec tests. With one Group Stream per group, the subscriber's wait for a duplicate stream (`Open::Duplicate`) is never reached. A stop's arrival against the header's acknowledgement is a free choice rather than modeled acknowledgements. A pass covers three starts.
