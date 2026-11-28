---
name: verify-credit
description: Model-check receive and send stream credit, pacing, paused budgets, and stuck-window eviction (`rs/moq-net/verify/credit/`) with TLA+. Use when changing flow-control, pacing, or credit-accounting code in `rs/moq-quic` or `rs/moq-net`.
---

# Verify credit

Two models, one per direction of a connection's flow control.

`rs/moq-net/verify/credit/ReceiveCredit.tla` models one connection's receive credit, from moq-quic's leases and stuck-window eviction to moq-net's ingest, which paces each group stream by its group's fastest reader. Bytes are units. The peer sends a unit once stream and connection credit allow, and it arrives in order. A reader leases what arrived and takes one chunk, at most to the next segment seal. It then hands the lease back: released at a seal, which returns the credit, or restored short of one, which keeps it. A grant is queued as moq-quic queues it, and the driver computes its limit when it builds the packet, at any later point (`Transmit`).

Ingest checks the group's pace gate, only at a seal. Past the pace window, the stream reserves the credit it holds from the session's paused budget (half the connection window) and parks until the reader catches up, the reader leaves, or the stream is evicted. A group no reader has picked up yet is owed to its track's subscribers, so its gate closes from the start, and opens once its readers leave or, before one picks it up, its track's last subscriber does. Refused a reservation, it reads on. A pause is charged what the stream holds once shaped to its slot's allowance (`cap / ((i + 1)(i + 2))`), and ingest shapes each stream's window to its group's room plus that allowance. Every moq-quic transport shapes; a transport that cannot (`Budgeted::unshaped`) has no stuck-window eviction to model.

When the peer has no connection credit left, moq-quic evicts the lowest-ranked stream holding credit, newest first. It holds off while an evicted reader has not stopped its stream, or while some reader has data it has not looked at, and reruns at every release, read, stop, and arrival, and at a restore when an earlier pass waited on that lease while it was out. A reader arrives late or leaves only under `Late` and `Leave`. Streams in `Plain` are read with `read()`, returning credit as they read. Under `Shrink` the window shrinks once, a debt the next returned credit pays before the peer sees any.

`rs/moq-net/verify/credit/SendCredit.tla` models the sender's wait for stream credit. Each subscription's `TrackRun` hands its groups out one at a time, in any arrival order. One group per subscription waits for credit (`TrackRun::opening`) in the session's line, ranked as `open::Rank` orders it. Only the head asks the transport, and a waiting open resets the lowest-ranked stream it preempts (`Lines::make_room`). While the waiting group is `stuck`, ranked below every stream its subscription serves (none included), the cursor hands out the group ranked highest by the policy it would go out at now, and only if it outranks the waiting one, which gives way (`give_way`): it is deferred, taking the place back once it empties or outranks the waiting group. Credit never gives a group up; only a budget does, which the model leaves out. Mixed ranges rank some groups at a lower-priority, oldest-first history range: `HistOnly` groups only it covers, and `Both` groups a live join covers until they age into it, oldest first. Under `Late`, groups arrive over time in any order. The receiver is abstract: a stream sent whole or reset is eventually freed, and ReceiveCredit.tla checks that it is. A group in `LiveSubs` at `LiveGroup` is one whose producer never finishes, so its stream keeps its credit until a higher-ranked open resets it, as in `rs/moq-net/tests/integration/stream_credit.rs`.

Each step is one poll of the code that handles one event, under the connection's lock where the code takes it. TLC checks every ordering under the bounds in each config.

## Rules

| Rule | Model | Holds when | Harm if broken |
| --- | --- | --- | --- |
| `Books` | Receive | Every unit arrived is held by one stream or was credited back. | Credit leaks, and the connection window shrinks for good. |
| `WithinWindow` | Receive | What all streams hold together (unread units, leased units not released, a frame partly in) stays within the connection window. | The peer is promised credit the receiver never returns: a flow-control error or a hang. |
| `PausedWithinCap` | Receive | Paused streams hold at most what they reserved, and that at most half the window. | Paused streams starve the rest of the connection's credit. |
| `EveryPauseFits` | Receive | No pause is refused under the cap, whatever order streams pause in and however late a group's first reader picks it up (`ReceiveCredit.cfg`, `ReceiveCreditSegments.cfg`, `ReceiveCreditWide.cfg`, and `ReceiveCreditThree.cfg`). | A stream that could pause reads on and sheds while a slow reader holds back the rest. |
| `ChargedAllowance` | Receive | A pause is charged no more than its slot's allowance (same configs). | The paused streams stop summing to under the cap, so a later one is refused. |
| `evict`'s assertion | Receive | Eviction never picks a stream whose reader may still progress (`Assert` in `EvictLoop`). | A reader that would have finished loses its group. |
| `Progress` and `Ends` | Receive | Every stream is read whole or evicted, and stopped (`AllEnd`). | A stream holds credit forever: the connection hangs. |
| `WithinCredit` | Send | Never more streams open than the peer granted. | The peer closes the connection with STREAM_LIMIT_ERROR. |
| `Progress` and `Ends` | Send | Every behavior ends quiet, and once quiet every group was sent and freed, is a live group's stream, or waits only on live streams it may not reset. | A subscription stops sending behind a group that never opens. |
| `TopDelivered` | Send | Once quiet, each subscription's highest-ranked group reached the peer, was reset for a higher priority, or waits only on live streams it may not reset. | A waiting group holds back a newer one that could reset an unfinished stream: the stall #8607 fixed. |
| `NoHeadOfLine` | Send | A subscription parked behind its stuck waiting group holds no group, cached or deferred, that outranks it. | History ranked below the waiting group holds back an urgent group behind it. |
| `DeferredWaits` | Send | A group is deferred only while another waits for credit. | A deferred group is never refilled, and the subscription ends without it. |

TLC's own liveness check is about a thousand times slower here, so liveness is checked through safety. `Progress` is an action property: every step other than the final stutter raises a bounded measure, so every behavior ends stuttering in one state. `Ends` is an invariant: every state is done, or one of the system's steps is enabled there, which weak fairness takes. `Ends` reads each step's guard in place of ENABLED, and `Guarded` checks the two agree on a smaller config. The comment above `Lifecycle` in ReceiveCredit.tla gives the measure.

The claim that every stream pauses, charged its slot's allowance, is `EveryPauseFits` and `ChargedAllowance`. It holds because a lease carries the window ingest shapes for the room its chunk leaves (`Lease::set_window`), and the hand back sets it under the same lock before queueing the grant (`HandBack`): set in a later lock hold, a driver on another thread could build the grant from the old window, and credit granted cannot be taken back.

Each model has witnesses (`NeverPaused`, `NeverEvicted`, `NeverStuckWaiting`, `NeverPartial`, `NeverPausedInside`; `NeverPreempts`, `NeverWaits`, `NeverBlocked`, `NeverDefers`). Each one is expected to fail, which shows TLC reaches the state. Check one after changing a step's guard.

## Run

```sh
quest gate bash sh/tla.sh rs/moq-net/verify/credit
quest gate just rs tla moq-net/verify/credit/ReceiveCredit.tla -config rs/moq-net/verify/credit/ReceiveCreditSegments.cfg
```

| Config | Covers | States | Time |
| --- | --- | --- | --- |
| `ReceiveCredit.cfg` | Two streams of two one-segment frames, a window two streams exhaust, readers late or leaving; also the allowance rules | 404k | 3 s |
| `ReceiveCreditMixed.cfg` | A plain-read stream beside two leased ones of one three-unit segment, under a window that may shrink to a quarter | 633k | 11 s |
| `ReceiveCreditSegments.cfg` | Frames of two segments, with readers late; also the allowance rules | 462k | 2 s |
| `ReceiveCreditWide.cfg` | A wider window, where the two slots' allowances differ; also the allowance rules | 143k | 3 s |
| `ReceiveCreditThree.cfg` | Three streams, readers from the start; also the allowance rules | 1.62M | 30 s |
| `ReceiveCreditGuards.cfg` | `Guarded`, on one-unit frames | 12.1k | 4 min |
| `ReceiveCreditMixedGuards.cfg` | `Guarded`, on one frame of a plain-read and a leased stream, with a shrink | 1.1k | 2 s |
| `SendCredit.cfg` | two subscriptions, one newest first and one oldest first, one stream of credit | 89k | 5 s |
| `SendCreditPriority.cfg` | one subscription at a higher priority, so its opens preempt; also `Guarded` | 99k | 2 min |
| `SendCreditStreams.cfg` | two streams of credit, the orders swapped | 300k | 17 s |
| `SendCreditLive.cfg` | a middle group of each subscription never finishes | 42.2k | 2 s |
| `SendCreditMixed.cfg` | one subscription's groups at mixed policies (history only, live aging into history, live only), arriving in any order; also `Guarded` | 4.3k | 9 s |

Every config passes. Nightly runs every config here and fails if a config with an `\* Open finding:` line passes.

The allowance rules hold however late a group's first reader picks it up, since a group nobody picked up yet is owed from the start: `Pace::room` is None, granting its stream a whole stream window, only once nobody is owed the group, and then no reader arrives to close the gate.

## Reading a failure

Follow Reading a failure in `rs/moq-net/verify/README.md`. Replay the path on the code:

- the moq-quic half with the stuck-window tests in `connection/streams/state.rs` (`stuck`, `lease_out_holds_eviction_off`, `evicted_taken_bytes_keep_credit_until_stop`);
- an ordering against the packet builder with `tests/sim.rs`;
- ingest's half with `Credited` in `coding/reader.rs` tests;
- the send half with the `Opener` tests in `transport/open.rs` and `TrackRun`'s tests in `lite/publisher.rs`.

In a receive trace, follow `pc`, `q[s]`'s `arr`, `taken`, `rel`, and `msd`, `charge`, and `slot` per stream.

## Coverage

Sync commit: `065a4c184340f7fc6035db97695327ef7d7e69a8`. Run `git log 065a4c1843.. -- rs/moq-quic/src/connection/streams rs/moq-net/src/coding/reader.rs rs/moq-net/src/transport/boxed.rs rs/moq-net/src/transport/open.rs rs/moq-net/src/model/pace.rs rs/moq-net/src/model/frame.rs rs/moq-net/src/model/group.rs rs/moq-net/src/lite/publisher.rs` to see what changed since. Follow Coverage in `rs/moq-net/verify/README.md` after a change to a covered function.

| Code | Model |
| --- | --- |
| `rs/moq-quic/src/connection/streams/state.rs` `StreamsState::received` | `Send` |
| `rs/moq-quic/src/connection/streams/state.rs` `StreamsState::write_control_frames`; `rs/moq-quic/src/connection/streams/recv.rs` `Recv::max_stream_data`, `Recv::can_send_flow_control`, `Recv::sent_max_stream_data`, `Recv::set_window`, `Recv::credit` | `Transmit`, `Limit`, `CanGrant`, `Credit` |
| `rs/moq-quic/src/connection/streams/lease.rs` `poll_lease`, `restore`, `release`, `queue_max_stream_data`, `Leased::release`, `Lease::set_window` | `Lease`, `HandBack`, `Queue`, `Shape` |
| `rs/moq-quic/src/connection/streams/lease.rs` `evict_if_stuck`, `trim_unlooked`; `rs/moq-quic/src/connection/streams/state.rs` `StreamsState::ranks`, `StreamsState::unlooked`, `StreamsState::stuck_waiting`, `StreamsState::evicted_held` | `Pass`, `EvictLoop`, `Evict`, `Unlooked`, `Order`, `stuckWaiting` |
| `rs/moq-quic/src/connection/streams/recv.rs` `Chunks::finalize_inner`; `rs/moq-quic/src/connection/streams/state.rs` `StreamsState::set_receive_window`, `StreamsState::add_read_credits` | `Read`, `ShrinkWindow`, `MaxData` |
| `rs/moq-net/src/coding/reader.rs` `Reader::poll_read_frame`, `Reader::poll_admit`, `Reader::poll_pace` | `Step`, `Park`, `Write` |
| `rs/moq-net/src/model/frame.rs` `Producer::poll_ready`, `Producer::unsealed`, `Producer::room`, `Producer::unsealed_after`; `rs/moq-net/src/model/pace.rs` `window`, `Pace::paced`, `Pace::ahead`, `Pace::took`, `Pace::park`, `Pace::room` | `Paced`, `Unsealed`, `PaceWindow`, `Take`, `Room` |
| `rs/moq-net/src/model/group.rs` `Producer::poll_paced`, `Producer::room`, `Handle::poll_unowed`, `Handle::owed`; `rs/moq-net/src/model/pace.rs` `Pace::hold`, `Pace::held` | `Paced`, `Room`, `Arrive`, `Drop` |
| `rs/moq-net/src/transport/boxed.rs` `Budgeted::window`, `Budgeted::pause`, `Budgeted::held`, `Budgeted::release`, `Shared::reserve`, `Pacing::cap`, `Pacing::allowance`, `Slots::take`, `Budgeted::shape`, `Budgeted::shaper`, `Shaper::window` | `PaceWindow`, `Charge`, `charge`, `Stop`, `Cap`, `Allowance`, `LowestFree`, `Shape` |
| `rs/moq-net/src/lite/publisher.rs` `TrackRun::opening`, `TrackRun::deferred`, `GroupServe::poll_open`, `TrackRun::stuck`, `TrackRun::give_way`, `TrackRun::take_deferred`, `GroupServe::defer`, `TrackRun::poll_step`, `TrackRun::poll_track`, `poll_policy` | `PollOpen`, `Refill`, `Stuck`, `Admitted`, `Outranking`, `Cursor`, `InGroup`, `Age`, `Step` |
| `rs/moq-net/src/model/track.rs` `Subscriber::poll_recv_group_above`, `Cursor::best_unread`, `Cursor::first_unread`; `rs/moq-net/src/model/resume.rs` `Reader::poll_first` | `Best`, `Outranking`, `Admitted` |
| `rs/moq-net/src/transport/open.rs` `Rank::preempts`, `Line::join`, `Line::leave`, `Line::wake_head`, `Line::resets`, `Lines::make_room`; `rs/moq-net/src/priority.rs` `in_group` | `Rank`, `Above`, `Join`, `Leave`, `Preempts`, `MakeRoom`, `InGroup` |

What the model relies on in that code:

- A stopped stream returns its paused credit and frees its slot (`Stop`), so a reader that never returns cannot pin the paused budget.
- A track hands every group to a subscriber that keeps reading, so a group owed to one is eventually picked up (`Arrive`, under weak fairness) unless the track's last subscriber leaves first (`Drop`). A subscriber that never picks a group up leaves it paused until it closes, which the model leaves out with a group's expiry.
- Eviction passes over a reader that may still progress, and an evicted reader's taken units keep their credit until it stops (`evicted_held`).
- Only the head of the line asks the transport, and leaving the line wakes the next head (`Line::leave`).
- A waiting group gives way to one that outranks it (`give_way`), and while it is `stuck` the cursor hands such a group out. Without either, `SendCreditLive.cfg` breaks `TopDelivered`, which the configs without a live group miss.
- The cursor's ranked read finds the best group as the best of each range's first group it still wants (`Cursor::best_unread`): within one range's run its groups rank in that range's order, and an older group is never fresher. The model takes the best of all at once (`Best`).
- With the admission before the ranked read, `SendCreditMixed.cfg` breaks `NoHeadOfLine`, a refused history group next in arrival order parking the cursor ahead of a live group that outranks the waiting one, and without that rule `Ends`, a `Both` group handed out by its live policy and shed by its aged rank.

## Not covered

Both models leave out loss, gaps, and reordering within a stream: every unit arrives in order. They also leave out rate caps, a want past a shrunk window (`TooLarge`), frame headers' bytes, and the peer resetting a stream on the receive model. Every stream ranks at one priority on the receive side, so eviction order is newest first. Relay chains are left out, since pausing is per hop. So are the front's shedding past a pause and a group's expiry. The send side has two priority levels (`High` and the rest), and within a mixed subscription a history range below its own. Only `Both` groups age, and a group ages at once into history, so a live budget past which a group goes stale, and the cursor's budget writing it off, are left out, as are retunes, which move ranks as aging does. TLC's simulate mode crashes on ReceiveCredit.tla (an `ArrayIndexOutOfBoundsException` in TLC), so larger shapes run exhaustively, as ReceiveCreditThree.cfg does. A reader that restores a lease and parks elsewhere while holding leased bytes is left out: `restore()` reruns eviction only when an earlier pass waited on that lease while it was out, so a reader that parks elsewhere must release or stop its stream. A pass covers two streams, or three with readers held from the start, of two or three frames, and two subscriptions of three groups, one of each live in `SendCreditLive.cfg`.
