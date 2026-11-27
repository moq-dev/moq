---
name: verify-subscribe
description: Model-check lite-07 subscription group accounting and a relay's N-to-1 deduplication across per-range Priority, Group Order, and Max Age (`rs/moq-net/verify/subscribe/`) with TLA+. Use when changing `SUBSCRIBE_DROP`/`SKIP`, the Max Age an OLD drop names, `GROUP_END`, group stream resets, preemption/expiry, per-group priority, demand folding (`demand/ranges.rs`), stale settlement and recovery at a relay, or subscriber tail in `rs/moq-net`.
---

# Verify subscription accounting

Two models: one subscription's group accounting, and many readers' ranges deduplicated into one upstream subscription.

`rs/moq-net/verify/subscribe/SubscribeAccounting.tla` models the groups of one answered lite-07 subscription between the publisher's `TrackRun` and the subscriber's `TrackServe`. The draft promises that every start in range gets a Group Stream, is accounted for by a GROUP_END, or is named in a SUBSCRIBE_DROP (with SKIP for declared skips, or an error code), and that every drop is final on its subscription, a stale (OLD) one included: a SUBSCRIBE_UPDATE reopens nothing, so the model leaves updates out. No skip goes out over a drop, but for the starts of a GROUP_END a reset may have cut. Drops and skips travel in order on the Subscribe Stream, and each Group Stream travels on its own. A Group Stream whose group expires stale resets with OLD, whether it never opened, is serving, or already sent its FIN and awaits the acknowledgement, so stale bytes never hold the send window; a subscriber that read it whole before the reset keeps the group. A stream reset while serving, evicted or preempted with TOO_FAR_BEHIND for a group that outranks it, is final. Under `ReliableReset` (a reset delivers its prefix: raw QUIC or QMux with RESET_STREAM_AT, never WebTransport), once the transport took a stream's GROUP header, a reset still delivers it, which accounts for the group, so no drop names it. Any other reset is named in a drop: before then the header never left, and a plain RESET_STREAM lets the peer discard it unread, or still deliver it beside the drop. The subscriber may stop a stream whose header it read, cutting what follows; a stop names the group in a drop only when it is EVICTED and beat the header's acknowledgement. A group may finish whole at an End, and no other group starts strictly inside its span. Its GROUP_END accounts for those starts, which the publisher then records as told, a drop of the group names the span once it is whole, and the track records the starts inside skipped, which the cursor skips where nothing told them yet. A reset or stop that still accounts for the group may cut its GROUP_END, so the publisher skips the starts it told again. Each model step is one poll of the code that handles one event. The track cursor's verdicts (hand a group out, give it up stale or final, skip it) are free choices, and so are when a stream fails and when a group finishes whole and where it ends. With `Pressure`, so is when the cache pool's debt folds the publisher's record of a group. TLC checks every ordering under the bounds in the config, and reports a deadlock for any state short of quiet where nothing can happen.

`rs/moq-net/verify/subscribe/SubscribeDedup.tla` models N readers at a relay, each with one range of its own Priority, Group Order, and Max Age (a live join at the edge, or a history range from the oldest group), fanned into one lite-07 upstream subscription. Readers join once and may leave once, so a narrow viewer comes and goes at any point. The relay folds their ranges into the upstream ranges, and a range folds into one covering it only when that loses no scheduling information: the other covers every group it does under a Max Age no narrower, at a higher priority, or an equal one with the same order or Descending. The publisher serves each group once: at the highest priority among the ranges covering it whose Max Age has not expired it, in that priority's order, Descending breaking ties, so a group a live join expired falls to a history range's priority. A group every covering range expired is OLD, named under the widest Max Age that covers it. With `Shed`, the publisher may also give a wanted group up for stream credit, as OLD named under the narrowest live join's Max Age (`0`, no limit, when there is none). The relay records a drop under exactly the Max Age the wire names; a reader no wider settles on it, and for a wider one the relay asks once on a request of its own with no Max Age, which delivers the group or drops it final. A group's age is how many groups landed above it, since only a landing above lets a clock convict one. Updates travel in order on the Subscribe Stream; group streams and drops travel unordered against them. Readers take from the copy in the step that changed it, since only the copy decides their outcome.

## Rules

Each rule is an INVARIANT, an action property (`[][...]_vars`, checked on every step), or an ASSUME; the harm says why it matters.

| Rule | Model | Holds when | Harm if broken |
| --- | --- | --- | --- |
| `ServedOnce` | Accounting | The publisher opens at most one Group Stream for a group. | The group's bytes go out twice, and the subscriber reads two streams of one group. |
| `NamedOnce` | Accounting | The publisher names a group in at most one drop and a start in at most one skip, and skips no start it served or dropped, besides the starts a reset cut out of a GROUP_END. A drop may follow the group's own Group Stream, even one sent whole, once it resets. | A second name contradicts a final one: the subscriber accounts the group twice or ends a reader on a group it delivered. |
| `NoContradictorySkip` | Accounting | No start a skip named has a Group Stream, and none inside a whole group's span does. | The subscriber records a start skipped while its group arrives, and a reader misses it. |
| `QuietIsSettled` | Accounting | Once nothing is in flight and the publisher has nothing left to do, the subscriber's tail accounts for every start, the ones inside a span whose GROUP_END a reset cut included. lite-07 has no grace, so this is the only way a start settles. | The subscriber waits forever for a group nobody will send, and a relay holds its readers there. |
| `SentOnce` | Dedup | A group's bytes go upstream at most once, whatever the readers ask: one stream on the subscription, or one request of the relay's own after the subscription dropped it stale. | Readers fanned into one subscription cost the upstream link once each, which is what the relay is for. |
| `NoPoison` | Dedup | A reader settles on a stale drop only when the verdict found the group older than the reader's own Max Age, or a shed named one no narrower. | A transient zero-max viewer makes a wider reader lose a group it could still get. |
| `QuietIsSettled` | Dedup | Once nothing is in flight and the publisher has nothing left to decide, every reader accounted for every group it is owed: the group, a final drop, or a stale one named under a Max Age no narrower than its own. Deadlock checking makes every behavior reach quiet. | A wider reader waits forever for a group the subscription dropped stale for a narrower one. |
| `EffectivePolicy` | Dedup | A group goes upstream at the highest priority among the readers' ranges covering it whose Max Age has not expired it, in that priority's order, as if nothing were folded (action property). | A live viewer's group waits behind history, or a group only history still wants keeps the live priority and sheds what the live viewer needs. |
| `JudgedWidest` | Dedup | A group is OLD only once every reader's range covering it expired it, and the drop names the widest Max Age among them (action property). | Readers that still want the group lose it, or wider ones ask again for a group the subscription could have sent. |
| `FoldRule` | Dedup | Folding loses nothing scheduled or judged by, on every mix of `FoldMix` ranges at `FoldPrios` in either order and every age vector, reachable or not (ASSUME; TLC exits 10 when false). | The upstream sees a different priority, order, or widest Max Age than its readers asked. |

## Run

```sh
quest gate just rs tla moq-net/verify/subscribe/SubscribeAccounting.tla
```

It checks every rule under `Pressure` and `ReliableReset` over three starts, 1 to 3, where group 1 may end at 2, 3, or 4, so its span holds none, one, or both of the starts above it: about 78.7M distinct states, 11 minutes. It passes. Nightly runs every config here and fails if a config with an `\* Open finding:` line passes; none has one. Each config's name starts with its model's. `SubscribeAccountingCutGroupEnd.cfg` checks a stream reset after its FIN, which delivers only its header and cuts the GROUP_END, so the publisher skips its starts again, over two starts without `Pressure` in a second:

```sh
quest gate just rs tla moq-net/verify/subscribe/SubscribeAccounting.tla -config rs/moq-net/verify/subscribe/SubscribeAccountingCutGroupEnd.cfg
```

`SubscribeAccountingPlainReset.cfg` checks a peer without RESET_STREAM_AT, or any WebTransport session (`ReliableReset = FALSE`), where every reset group is named in a drop that may land beside a header the subscriber read, over three starts without `Pressure`: about 32.2M distinct states, 4 minutes. It passes.

```sh
quest gate just rs tla moq-net/verify/subscribe/SubscribeAccounting.tla -config rs/moq-net/verify/subscribe/SubscribeAccountingPlainReset.cfg
```

The dedup model, quick config first while iterating:

```sh
quest gate just rs tla moq-net/verify/subscribe/SubscribeDedup.tla -config rs/moq-net/verify/subscribe/SubscribeDedupQuick.cfg
quest gate just rs tla moq-net/verify/subscribe/SubscribeDedup.tla
quest gate just rs tla moq-net/verify/subscribe/SubscribeDedup.tla -config rs/moq-net/verify/subscribe/SubscribeDedupThree.cfg
```

| Config | Covers | States | Time |
| --- | --- | --- | --- |
| `SubscribeDedupQuick.cfg` | Two readers over three groups at two priorities, newest first, Max Ages 0 and none; `FoldRule` on pairs | 3.31M | 26 s |
| `SubscribeDedup.cfg` | Two readers over three groups at two priorities in either order, Max Ages 0, 1, and none; `FoldRule` on every three ranges at three priorities | 25.1M | 4 min |
| `SubscribeDedupThree.cfg` | Three readers over three groups at one priority and order, so Max Ages 0, 1, and none share one subscription | 83.0M | 14 min |

Every config passes. Each rule was broken once to see TLC catch it (see Coverage).

## Reading a failure

Follow Reading a failure in `rs/moq-net/verify/README.md`. Replay the path on the code: the publisher's half with `lite07_run`, `write_group`, `forward`, `drops`, and `SinkSession::with_unacked_fin` in `lite/publisher.rs` tests; the subscriber's half with `lite07_serving`, `apply_response`, and `GroupRecv` in `lite/subscriber.rs` tests. Replay a dedup path at a relay with `tests/integration/widened_max_age.rs`, whose readers join and narrow behind one relay, and the Max Age the publisher names on a drop with `lite07_end_precedes_blocked_streams_and_reports_expiry` in `lite/publisher.rs` tests. In a dedup trace, follow `pubR` against `pubSnap` for what the fold dropped, `verdict` for each group's decision and the Max Age it named, and `copy`, `rm`, and `how` for what each reader took.

## Coverage

### SubscribeAccounting.tla

Sync commit: `f7a41c3ea52fa6b12c6bf698142f428c24ea673b`. Run `git log f7a41c3ea5.. -- rs/moq-net/src/lite/publisher.rs rs/moq-net/src/lite/subscriber.rs rs/moq-net/src/tail.rs rs/moq-net/src/model/track.rs rs/moq-net/src/model/record.rs` to see what changed since. Follow Coverage in `rs/moq-net/verify/README.md` after a change to a covered function.

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
- A reset or stop after a GROUP_END that still accounts for the group skips the starts the GROUP_END told again (`GroupOutcomes::cut`), since it may cut the GROUP_END on the wire (`QuietIsSettled`, `SubscribeAccountingCutGroupEnd.cfg`). Those starts may include one dropped before the group finished, which the skip changes nothing for, so `NamedOnce` exempts them.
- A stream reset before the transport took its header is named in a drop (`accounts`), since the reset loses the header (`QuietIsSettled`).
- A stream reset without RESET_STREAM_AT, or over WebTransport, is named in a drop even after the transport took its header, since the peer may discard it unread (`GroupContext::reliable_reset`, `QuietIsSettled`, `SubscribeAccountingPlainReset.cfg`).

### SubscribeDedup.tla

Sync commit: `f7a41c3ea52fa6b12c6bf698142f428c24ea673b`. Run `git log f7a41c3ea5.. -- rs/moq-net/src/model/track/demand rs/moq-net/src/model/track/demand.rs rs/moq-net/src/lite/subscribe.rs rs/moq-net/src/lite/publisher.rs rs/moq-net/src/lite/subscriber.rs rs/moq-net/src/lite/subscriber/fills.rs rs/moq-net/src/model/subscription.rs rs/moq-net/src/model/subscription/window.rs rs/moq-net/src/model/group.rs` to see what changed since.

| Code | Model |
| --- | --- |
| `rs/moq-net/src/model/track/demand/ranges.rs` `Ranges::union`, `Ranges::fold`; `rs/moq-net/src/model/track/demand.rs` `Subscriptions::combined` | `Folds`, `Fold`, `Update`, `FoldRule` |
| `rs/moq-net/src/lite/publisher.rs` `Steer::serving`, `poll_policy`, `rank_at`; `rs/moq-net/src/model/subscription.rs` `Policy::top`, `Policy::covers` | `PubPrio`, `PubOrder`, `TopPrio`, `TopOrder` |
| `rs/moq-net/src/lite/publisher.rs` `TrackRun::poll_track` (`Recv::Group`), `TrackRun::serve`, `TrackRun::mark_served` | `Send`, `told` |
| `rs/moq-net/src/model/subscription/window.rs` `Budget::at`; `rs/moq-net/src/model/track.rs` `GroupExpiry::is_expired`; `rs/moq-net/src/model/group.rs` `Consumer::expired_under`; `rs/moq-net/src/lite/publisher.rs` `GroupServe::settle_failed` | `Wanting`, `Widest`, `PubJudged`, `Judge` |
| `rs/moq-net/src/lite/publisher.rs` `TrackRun::hole`, `TrackRun::give_way`, `TrackRun::shed`; `rs/moq-net/src/model/subscription.rs` `Subscription::narrowest`; `rs/moq-net/src/lite/subscribe.rs` `SubscribeDrop::given_up`, `SubscribeDrop::stale` | `GiveUp`, `Narrowest`, `PubUnjudged`, `verdict` |
| `rs/moq-net/src/lite/subscriber.rs` `ServeLoop::apply_response` (`Drop` with `OLD`) | `Arrive`, `rm` |
| `rs/moq-net/src/model/track.rs` `settles`, `Drift::settles` | `Take` |
| `rs/moq-net/src/lite/subscriber/fills.rs` `RangeFills`, `Recovery::new` | `Serve` (`recov`), `RecoverAnswer` |

What the model asks of that code, each found by TLC as a broken rule without it:

- The publisher serves no group its record already told, however many upstream ranges cover it (`Record::told`, `SentOnce`).
- The relay records a stale drop under the Max Age the wire names, not a budget of its own (`apply_response`, `NoPoison`). Recorded under its widest reader's, a shed named under a live viewer's Max Age 0 settles a history reader the group never expired for.
- A reader wider than a stale record gets the group on a request of the relay's own (`Recovery`, `QuietIsSettled`).
- A group's priority comes only from the ranges that have not expired it (`EffectivePolicy`): otherwise a group a high-priority live join expired still goes at the live priority while only a low-priority history range wants it.
- A group is OLD only once every covering range expired it (`JudgedWidest`).
- A range folds into one covering it only at a priority and order that decide every group it wants as it would (`FoldRule`); folding every range into a held `Range::ALL` regardless breaks it.

## Not covered

SubscribeAccounting.tla: the subscription starts answered and covers the groups, so SUBSCRIBE_START, ranges and Range IDs, fills (`RangeFills`) and the separate request (`Recovery`) a reader wider than a stale drop asks with (SubscribeDedup.tla's `RecoverAnswer`), floors, SUBSCRIBE_UPDATE, and SUBSCRIBE_END with `drop_unserved` are left out, and so are the probe ranges a relay copy asks upstream for a time start it cannot prove (`Kind::Probe`), which an answer's SUBSCRIBE_DROP and SUBSCRIBE_START treat as a fill. A group a relay fetches the head of before serving (`fetches_from`) is the free choices already here: its stream opens late, or it is named in a drop once the fetch is refused. A copy's cache sheds frames under the pool and a session's live-edge budget (`shed_edge`), which `Evict` and a failing stream stand for. The ledger's folding is left out, and lite-07 has no grace: `QuietIsSettled` asks for the publisher's answer. So are priority and order (`Steer`), which stream a preempt picks, the Subscribe Stream's FIN and cancellation, sessions closing, versions before lite-07, relays forwarding drops downstream, the budget a relay records a stale drop under (SubscribeDedup.tla's), datagrams, and the copy's readers (`poll_drop`). A route switch appears only as the cursor handing out once more a group it gave up. FETCH is left out: one request and one response, ended by a FIN or a reset, with nothing to race. GROUP_END takeover is left out, and so are subscriber-side drain of an already-buffered GROUP_END on local abort (`FrameIngest::try_drain_end`) and frames: an End is at most one past the highest start, so a span may run past the range, and its delta coding is left to the codec tests. With one Group Stream per group, the subscriber's wait for a duplicate stream (`Open::Duplicate`) is never reached. A stop's arrival against the header's acknowledgement is a free choice rather than modeled acknowledgements. A pass covers three starts.

SubscribeDedup.tla: each reader holds one range, live joins land at the edge with no lag, and history ranges run from the oldest group, so time starts, lags, placed joins (`Subscriptions::combined`'s `placed`), probes, and retunes are left out; a reader with several ranges is settled per covering budget, so it stands as several readers. Groups go whole: stream resets, GROUP_END, and skips are SubscribeAccounting.tla's, and the send queue's order between groups (`stuck`, preemption) is SendCredit.tla's, so a shed is a free choice. The relay never evicts a group it holds, so a later reader never needs a second fetch, and its readers never judge a group stale on their own clock. `settles`' further conditions (a reader reading every group above the run, and the upstream's cap) only settle fewer readers and are left out. The publisher's max age is unbounded, so its clamp (`max_age_bound`) and a verdict at it are left out, as are fills riding the subscription (`FillAsk::merge`), relay chains, and the Subscribe Stream's FIN. A group ages once per group landing above it, rather than on a clock. Only the newest relay update is in flight: the publisher's steps and the relay's joins and leaves touch nothing of each other's, so each update applied in turn reaches no other state. A shed names the narrowest live join's Max Age the publisher sees, which a fold of a narrower live join into a wider one raises, and with no finite live join it names `0` and settles every reader, history ones included; `NoPoison` takes either as a congestion loss. A pass covers three readers at one policy, or two at every policy mix, over three groups.
