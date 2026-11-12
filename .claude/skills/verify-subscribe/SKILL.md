---
name: verify-subscribe
description: Model-check how a lite-07 subscription accounts for each group (`rs/moq-net/src/lite/publisher.rs` `TrackRun` and `GroupServe`, `lite/subscriber.rs` `ServeLoop::apply_response` and `buffer_update`, `src/tail.rs`) with TLA+. Use when a change touches SUBSCRIBE_DROP, SUBSCRIBE_SKIP, stale (OLD) drops and their update numbers, SUBSCRIBE_UPDATE re-offers, Group Stream resets and expiry, duplicate Group Streams, or the subscriber's tail, or when a drop or re-offer bug needs every ordering of the Subscribe Stream against the Group Streams checked.
---

# Verify subscription accounting

`rs/moq-net/verify/subscribe/SubscribeAccounting.tla` models one group of one answered lite-07 subscription between the publisher's `TrackRun` and the subscriber's `TrackServe`. The draft promises that every sequence in range gets a Group Stream, a SUBSCRIBE_SKIP, or a SUBSCRIBE_DROP; that a stale (OLD) drop holds only under the update it names and the next SUBSCRIBE_UPDATE reopens it; and that a Group Stream wins over a stale drop, even one it overtook. Drops and skips travel in order on the Subscribe Stream, each Group Stream travels on its own, a reset can lose a stream's header, and a reset sent after the FIN may arrive after every byte. Each model step is one poll of the code that handles one event. The track cursor's verdicts (hand the group out, give it up stale or final, skip it) and whether the cache still holds a fresh copy on an update are free choices. TLC checks every ordering under the bounds in the config.

## Rules

Each rule is an INVARIANT; the harm says why it matters.

| Rule | Holds when | Harm if broken |
| --- | --- | --- |
| `NoViolation` | No SUBSCRIBE_DROP names an update the subscriber has not sent. | The subscriber closes the session with a PROTOCOL_VIOLATION. |
| `ServedOnce` | The publisher opens a second Group Stream for a group only after resetting the first. | The group's bytes go out twice at once. |
| `QuietIsSettled` | Once nothing is in flight and the publisher has nothing left to do, the subscriber's tail accounts for the group. | The subscriber waits out its grace for a group nobody will send, and a relay holds its readers there. |
| `StreamWins` | A group the subscriber read to its FIN is never recorded stale. | The subscriber owes a group it already has; the next update reopens it, and the publisher sends the whole group again or drops it again on every update. |

## Run

```sh
just rs tla moq-net/verify/subscribe/SubscribeAccounting.tla
just rs tla moq-net/verify/subscribe/SubscribeAccounting.tla -config rs/moq-net/verify/subscribe/StreamWins.cfg
```

The first checks every rule but `StreamWins`: about 3.0M states, in seconds, and it passes at the sync commit. The second fails at the sync commit on the open finding below. `just test tla rs/moq-net/verify/subscribe` runs every config here, as Nightly does, and fails if a config with an `\* Open finding:` line passes. One group is enough: groups share only the order of the Subscribe Stream, and a second group multiplies the states (60M and more) without adding a race.

## Open finding: a group read whole is named in a stale drop

`StreamWins.cfg` fails. `GroupServe` in `GroupState::Closed` has written every frame and the FIN, and resets with OLD when the group goes stale before the acknowledgement; `settle_failed` then has `TrackRun::poll_step` name it in a stale drop under the update in effect, and record it in `expired`. The subscriber may already hold every byte. It ignores that drop only while the group is in `Tail::finished`, which `buffer_update` clears through `Tail::updated`, or while its copy still caches the group (`track::Producer::give_up` keeps an open group). So with an update sent between reading the FIN and the drop's arrival, and the copy having evicted the group, the subscriber records it stale and owes it again on the next update, while that update makes the publisher serve the whole group again (`reoffer_groups`) or drop it again under every later update. The draft says the publisher, having served a group, "neither serves it again nor names it in another drop". `residual_a_stale_unacknowledged_fin_drops_and_resends_a_served_group` in `lite/publisher.rs` replays the publisher's half, and `a_finished_group_wins_over_a_late_stale_drop_once_evicted` in `lite/subscriber.rs` pins the subscriber's half. Keeping `Tail::finished` across updates passes every rule in the model (about 1.9M states). Once it is fixed, flip the residual test, move `StreamWins` into `SubscribeAccounting.cfg`, and delete `StreamWins.cfg`.

## Reading a failure

Follow Reading a failure in `rs/moq-net/verify/README.md`. Replay the path on the code: the publisher's half with `lite07_run`, `write_group`, `run.update`, `SinkSession::with_unacked_fin`, and `drop_updates` in `lite/publisher.rs` tests; the subscriber's half with `lite07_serving`, `apply_response`, `buffer_update`, and `GroupRecv` in `lite/subscriber.rs` tests.

## Coverage

Sync commit: `103052ed074a87f00269ecb0d639905482ecfb6f`. Run `git log 103052ed07.. -- rs/moq-net/src/lite/publisher.rs rs/moq-net/src/lite/subscriber.rs rs/moq-net/src/tail.rs rs/moq-net/src/model/track.rs` to see what changed since. Follow Coverage in `rs/moq-net/verify/README.md` after a change to a covered function.

| Code | Model |
| --- | --- |
| `rs/moq-net/src/lite/publisher.rs` `TrackRun::poll_track` (`Recv::Group`, the re-offer pop), `serve`, `mark_served` | `CursorHand`, `PubReoffer`, `Serve` |
| `rs/moq-net/src/lite/publisher.rs` `poll_forward_drop`, `forward_range`, `drop_range`, `replace_stale`, `sent_stale`, `hole` | `CursorGiveUp`, `ReplaceStale` |
| `rs/moq-net/src/lite/publisher.rs` `TrackRun::poll_step` (outcomes, `redrop`) | `PubOutcomes`, `PubRedrop` |
| `rs/moq-net/src/lite/publisher.rs` `TrackRun::update`, `owe`; `rs/moq-net/src/model/track.rs` `reoffer_stale`, `reoffer_groups` | `PubUpdate` |
| `rs/moq-net/src/lite/publisher.rs` `GroupServe::poll_serve` (`Serve`, `Closed`), `settle_failed` | `StreamFin`, `StreamFail`, `StreamAck` |
| `rs/moq-net/src/lite/subscriber.rs` `TrackEntry::open`, `reopen`, `place`; `GroupRecv::poll_serve` (the duplicate wait) | `SubHeader`, `SubFin`, `SubReset`, `SubResetHeld` |
| `rs/moq-net/src/lite/subscriber.rs` `ServeLoop::apply_response` (`Drop`, `Skip`) | `OnDrop`, `OnSkip`, `SubRecv` |
| `rs/moq-net/src/lite/subscriber.rs` `buffer_update`; `rs/moq-net/src/model/track.rs` `supersede_stale` | `SubUpdate` |
| `rs/moq-net/src/tail.rs` `Reading::open`, `Reading::finish`, `Tail::finished`, `updated`, `account`, `unaccount` | `acct`, `finished` |
| `rs/moq-net/src/model/track.rs` `Producer::give_up`, `skip_received` | `rec`, `cached` |

## Not covered

The subscription starts answered and covers the group, so SUBSCRIBE_OK, SUBSCRIBE_STARTED, ranges and Range IDs, fills (`RangeFills`), floors, and SUBSCRIBE_FINISH with `drop_unserved` are left out. The tail's grace and the ledger's folding are left out: `QuietIsSettled` asks for the publisher's answer, never the grace. So are priority and order (`Steer`), the Subscribe Stream's FIN and cancellation, sessions closing, versions before lite-07, relays forwarding drops downstream, datagrams, and the copy's readers (`poll_drop`). A route switch appears only as the cursor handing out once more a group it gave up. FETCH is left out: one request and one response, ended by a FIN or a reset, with nothing to race. A pass covers one group, four updates, and four Group Streams.
