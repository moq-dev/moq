---
name: verify-subscribe
description: Model-check how a lite-07 subscription accounts for each group (`rs/moq-net/src/lite/publisher.rs` `TrackRun` and `GroupServe`, `lite/subscriber.rs` `ServeLoop::apply_response` and `buffer_update`, `src/tail.rs`) with TLA+. Use when a change touches SUBSCRIBE_DROP, SUBSCRIBE_SKIP, stale (OLD) drops and their update numbers, SUBSCRIBE_UPDATE re-offers, Group Stream resets and expiry, YIELDED resets and continuations of a paused group, duplicate Group Streams, or the subscriber's tail, or when a drop or re-offer bug needs every ordering of the Subscribe Stream against the Group Streams checked.
---

# Verify subscription accounting

`rs/moq-net/verify/subscribe/SubscribeAccounting.tla` models one group of one answered lite-07 subscription between the publisher's `TrackRun` and the subscriber's `TrackServe`. The draft promises that every sequence in range gets a Group Stream, a SUBSCRIBE_SKIP, or a SUBSCRIBE_DROP; that a stale (OLD) drop holds only under the update it names and the next SUBSCRIBE_UPDATE reopens it; and that a Group Stream wins over a stale drop, even one it overtook. Drops and skips travel in order on the Subscribe Stream, each Group Stream travels on its own, a reset can lose a stream's header, and a reset sent after the FIN may arrive after every byte. A publisher out of stream credit may reset a Group Stream with YIELDED, which pauses the group instead of accounting for it: the subscriber keeps the frames it read, and the publisher owes a continuation, a Group Stream that starts at the reset stream's own Frame Start, or a drop. A continuation may overtake the YIELDED reset, or even the header, of the stream it continues. Each model step is one poll of the code that handles one event. The track cursor's verdicts (hand the group out, give it up stale or final, skip it) and whether the cache still holds a fresh copy on an update are free choices, and so are when the publisher yields a stream and whether it continues a paused group or gives it up. With `Pressure`, so is when the cache pool's debt folds what each side keeps of the group: the publisher's record and the subscriber's tail. TLC checks every ordering under the bounds in the config.

## Rules

Each rule is an INVARIANT; the harm says why it matters.

| Rule | Holds when | Harm if broken |
| --- | --- | --- |
| `NoViolation` | No SUBSCRIBE_DROP names an update the subscriber has not sent. | The subscriber closes the session with a PROTOCOL_VIOLATION. |
| `ServedOnce` | The publisher opens a second Group Stream for a group only after resetting the first, and a continuation continues the group's latest stream, which it reset with YIELDED. | The group's bytes go out twice at once, or a continuation resumes a group nobody paused. |
| `QuietIsSettled` | Once nothing is in flight and the publisher has nothing left to do, the subscriber's tail accounts for the group. | The subscriber waits out its grace for a group nobody will send, and a relay holds its readers there. |
| `PausedIsOwed` | While the publisher owes a paused group, the subscriber's paused copy stays owed, unless a drop or skip under the update in effect, a stream read to its FIN, or a reset of another code that arrived with its header accounts for it. | The tail settles on a YIELDED reset alone and the subscription ends before the continuation arrives. |
| `ContinuesWithinHeld` | A continuation that takes over a paused copy starts no later than the frames the copy holds (`Continuation.cfg`). | The subscriber treats the continuation as malformed, or a frame a reset discarded is never delivered. |
| `OneReader` | The subscriber reads at most one Group Stream of a group at a time; a continuation waits, unread, for the stream it continues to end. | Two streams write one group's frames at once. |
| `StreamWins` | A group the subscriber read to its FIN is never recorded stale. | The subscriber owes a group it already has; the next update reopens it, and the publisher sends the whole group again or drops it again on every update. |

## Run

```sh
quest gate just rs tla moq-net/verify/subscribe/SubscribeAccounting.tla
quest gate just rs tla moq-net/verify/subscribe/SubscribeAccounting.tla -config rs/moq-net/verify/subscribe/Continuation.cfg
quest gate just rs tla moq-net/verify/subscribe/SubscribeAccounting.tla -config rs/moq-net/verify/subscribe/StreamWins.cfg
```

The first checks every rule but `StreamWins` and `ContinuesWithinHeld` under `Pressure`, with one YIELDED reset and frames left out: about 77.7M distinct states, fifteen minutes. The second gives the group one frame and two YIELDED resets, without `Pressure` and over two updates, so it also checks where each continuation starts: about 29.6M distinct states, five minutes. Both pass. The third checks `StreamWins` without `Pressure` or YIELDED and fails on the open finding below. Nightly runs every config here and fails if a config with an `\* Open finding:` line passes. One group is enough: groups share only the order of the Subscribe Stream, and a second group multiplies the states (60M and more) without adding a race.

## Open finding: a group read whole is named in a stale drop

`StreamWins.cfg` fails. `GroupServe` in `GroupState::Closed` has written every frame and the FIN, and resets with OLD when the group goes stale before the acknowledgement; `settle_failed` then has `TrackRun::poll_step` name it in a stale drop under the update in effect, and record it in `expired`. The subscriber may already hold every byte. It ignores that drop only while the group is in `Tail::finished`, which `buffer_update` clears through `Tail::updated`, or while its copy still caches the group (`track::Producer::give_up` keeps an open group). So with an update sent between reading the FIN and the drop's arrival, and the copy having evicted the group, the subscriber records it stale and owes it again on the next update, while that update makes the publisher serve the whole group again (`reoffer_groups`) or drop it again under every later update. The draft says the publisher, having served a group, "neither serves it again nor names it in another drop". `residual_a_stale_unacknowledged_fin_drops_and_resends_a_served_group` in `lite/publisher.rs` replays the publisher's half, and `a_finished_group_wins_over_a_late_stale_drop_once_evicted` in `lite/subscriber.rs` pins the subscriber's half. Keeping `Tail::finished` across updates passes every rule in the model without `Pressure` (about 1.9M states). Under it, `Tail::rebill` still forgets a finished run by design: the overtaken drop records the group stale, though `Tail::overtaken` keeps it out of what the next update owes, since the ledger accounted the group before the drop. Once it is fixed, flip the residual test and drop the `\* Open finding:` line from `StreamWins.cfg`, which stays without `Pressure`.

## Reading a failure

Follow Reading a failure in `rs/moq-net/verify/README.md`. Replay the path on the code: the publisher's half with `lite07_run`, `write_group`, `run.update`, `SinkSession::with_unacked_fin`, and `drop_updates` in `lite/publisher.rs` tests; the subscriber's half with `lite07_serving`, `apply_response`, `buffer_update`, and `GroupRecv` in `lite/subscriber.rs` tests.

## Coverage

Sync commit: `6aa4f9c1cad482aa86adcf3cf26bd7779697a01a`. Run `git log 6aa4f9c1ca.. -- rs/moq-net/src/lite/publisher.rs rs/moq-net/src/lite/subscriber.rs rs/moq-net/src/tail.rs rs/moq-net/src/model/track.rs` to see what changed since. Follow Coverage in `rs/moq-net/verify/README.md` after a change to a covered function.

| Code | Model |
| --- | --- |
| `rs/moq-net/src/lite/publisher.rs` `TrackRun::poll_track` (`Recv::Group`, the re-offer pop), `serve`, `mark_served` | `CursorHand`, `PubReoffer`, `Serve` |
| `rs/moq-net/src/lite/publisher.rs` `TrackRun::give_way`, `shed` | a new group, or one shed, as `Serve`, then `StreamFail` with `old` whose header never arrives; a continuation as `StreamYield` of a stream whose header never arrives |
| `rs/moq-net/src/lite/publisher.rs` `poll_forward_drop`, `forward_range`, `drop_range`, `replace_stale`, `sent_stale`, `hole` | `CursorGiveUp`, `ReplaceStale` |
| `rs/moq-net/src/lite/publisher.rs` `TrackRun::poll_step` (outcomes, `redrop`) | `PubOutcomes`, `PubRedrop` |
| `rs/moq-net/src/lite/publisher.rs` `TrackRun::update`, `owe`; `rs/moq-net/src/model/track.rs` `reoffer_stale`, `reoffer_groups` | `PubUpdate` |
| `rs/moq-net/src/lite/publisher.rs` `GroupServe::poll_serve` (`Serve`, `Closed`), `settle_failed` | `StreamFin`, `StreamFail`, `StreamAck` |
| `rs/moq-net/src/lite/subscriber.rs` `TrackEntry::open`, `reopen`, `place`; `GroupRecv::poll_serve` (the duplicate wait) | `SubHeader`, `SubFin`, `SubReset`, `SubResetHeld` |
| `rs/moq-net/src/lite/subscriber.rs` `ServeLoop::apply_response` (`Drop`, `Skip`) | `OnDrop`, `OnSkip`, `SubRecv` |
| `rs/moq-net/src/lite/subscriber.rs` `buffer_update`; `rs/moq-net/src/model/track.rs` `supersede_stale` | `SubUpdate` |
| `rs/moq-net/src/tail.rs` `Reading::open`, `Reading::finish`, `Tail::finished`, `updated`, `account`, `unaccount` | `acct`, `finished` |
| `rs/moq-net/src/tail.rs` `Tail::rebill`, `overtaken`, `updated`; `rs/moq-net/src/lite/subscriber.rs` `ServeLoop::apply_response`; `rs/moq-net/src/model/record.rs` `Record::folded` | `TailFold`, `forgot`, `PubFold`, `folded` |
| `rs/moq-net/src/model/track.rs` `Producer::give_up`, `skip_received` | `rec`, `cached` |
| `rs/moq-net/src/lite/publisher.rs` `GroupServe::settle_failed`, `TrackRun::open`; `rs/moq-net/src/transport/boxed.rs` `SendStream::preempted` | `StreamYield`, `PubContinue`, `PausedGiveUp` |
| `rs/moq-net/src/lite/subscriber.rs` `TrackEntry::pause`, `resume`, `dropped`, `read_yielding`, `end_yielding`, `YieldVerdict`, `GroupRecv::poll_serve`, `FrameIngest::dispatch` | the YIELDED branch of `SubReset`, `SubFrame`, `reading`, `sdrop`, `paused` |

What the model asks of that code, each found by TLC as a broken rule without it:

- A YIELDED reset takes back only what its own stream's header added to the ledger (`reading`): `TrackEntry::open` reads whether the tail already accounted the group before the stream accounts it (`Yielding::owed`). A header that arrives once the ledger settled the group adds nothing, so a YIELDED stream whose header arrives after its continuation finished the group owes nothing again (`QuietIsSettled`).
- A continuation that takes over from a held position inherits that account (`TrackEntry::resume`), so its own YIELDED reset takes it back.
- A drop under the update in effect that reached a stream still being read ends its group once the YIELDED reset arrives (`TrackEntry::dropped`). The model then records the group stale; the code aborts the group with the drop's error, which the drop already accounted, so the tail settles either way.
- A stream waits, held, while another stream of its group is still read, even one a final drop aborted, not only while the copy is open (`OneReader`).

## Not covered

The subscription starts answered and covers the group, so SUBSCRIBE_OK, SUBSCRIBE_STARTED, ranges and Range IDs, fills (`RangeFills`), floors, and SUBSCRIBE_FINISH with `drop_unserved` are left out. The tail's grace and the ledger's folding are left out, so a group `Tail::overtaken` reports is one whose header arrived: `QuietIsSettled` asks for the publisher's answer, never the grace. So are priority and order (`Steer`), the Subscribe Stream's FIN and cancellation, sessions closing, versions before lite-07, relays forwarding drops downstream, datagrams, and the copy's readers (`poll_drop`). A route switch appears only as the cursor handing out once more a group it gave up. FETCH is left out: one request and one response, ended by a FIN or a reset, with nothing to race. GROUP_END takeover is left out: only the YIELDED reset pauses a group, and the publisher yields only a stream still serving. A pass covers one group, four updates, four Group Streams, and one YIELDED reset, or two updates, two YIELDED resets, and one frame in `Continuation.cfg`.
