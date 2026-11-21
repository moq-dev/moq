---
name: verify-credit
description: Model-check receive and send credit (`rs/moq-quic/src/connection/streams/lease.rs` leases and `evict_if_stuck`, `rs/moq-net/src/coding/reader.rs` pacing, `transport/boxed.rs` pausing, `lite/publisher.rs` `TrackRun::opening`, `transport/open.rs`) with TLA+. Use when a change touches how a lease returns credit, `MAX_STREAM_DATA`, stuck-window eviction (`evicted_held`, `trim_unlooked`, `unlooked`), the pace gate or where ingest pauses a stream, the paused budget (`Budgeted::pause`, `reserve`, the half-window cap, slot allowances, `set_window`, `shape`), or how a group waits for stream credit (`Lines::make_room`, `give_way`, `shed`, `stuck`), or when a credit hang needs every ordering of arrivals, reads, pauses, and evictions checked rather than a random sweep.
---

# Verify credit

Two models, one per direction of a connection's flow control.

`rs/moq-net/verify/credit/ReceiveCredit.tla` models one connection's receive credit, from moq-quic's leases and stuck-window eviction to moq-net's ingest, which paces each group stream by its group's fastest reader. Bytes are units. The peer sends a unit once stream and connection credit allow, and it arrives in order. A reader leases what arrived and takes one chunk, at most to the next segment seal. It then hands the lease back: released at a seal, which returns the credit, or restored short of one, which keeps it. A grant is queued as moq-quic queues it, and the driver computes its limit when it builds the packet, at any later point (`Transmit`).

Ingest checks the group's pace gate. Past the pace window, the stream reserves the credit it holds from the session's paused budget (half the connection window) and parks until the reader catches up, the reader leaves, or the stream is evicted. Refused a reservation, it reads on. `Shaped` selects moq-dev/moq#9210's design, `FALSE` main's:

- #9210 gates only at a seal, charges a pause what the stream holds once shaped to its slot's allowance (`cap / ((i + 1)(i + 2))`), and shapes each stream's window to its group's room plus that allowance.
- Main gates on every turn and charges a stream window plus what the stream took but has not released.

When the peer has no connection credit left, moq-quic evicts the lowest-ranked stream holding credit, newest first. It holds off while an evicted reader has not stopped its stream, or while some reader has data it has not looked at. A reader arrives late or leaves only under `Late` and `Leave`.

`rs/moq-net/verify/credit/SendCredit.tla` models the sender's wait for stream credit. Each subscription's `TrackRun` hands its groups out one at a time, in any arrival order. One group per subscription waits for credit (`TrackRun::opening`) in the session's line, ranked as `open::Rank` orders it. Only the head asks the transport, and a waiting open resets the lowest-ranked stream it preempts (`Lines::make_room`). A group handed out while another waits replaces it if it outranks it (`give_way`), and is shed otherwise. While the waiting group is `stuck`, the cursor hands out only groups that may move it on. The receiver is abstract: a stream sent whole or reset is eventually freed, and ReceiveCredit.tla checks that it is. A group in `LiveSubs` at `LiveGroup` is one whose producer never finishes, so its stream keeps its credit until a higher-ranked open resets it, as in `rs/moq-net/tests/integration/stream_credit.rs`.

Each step is one poll of the code that handles one event, under the connection's lock where the code takes it. TLC checks every ordering under the bounds in each config.

## Rules

| Rule | Model | Holds when | Harm if broken |
| --- | --- | --- | --- |
| `Books` | Receive | Every unit arrived is held by one stream or was credited back. | Credit leaks, and the connection window shrinks for good. |
| `WithinWindow` | Receive | What all streams hold together (unread units, leased units not released, a frame partly in) stays within the connection window. | The peer is promised credit the receiver never returns: a flow-control error or a hang. |
| `PausedWithinCap` | Receive | Paused streams hold at most what they reserved, and that at most half the window. | Paused streams starve the rest of the connection's credit. |
| `EveryPauseFits` | Receive | No pause is refused under the cap, whatever order streams pause in (`ReceiveCreditWide.cfg` and `ReceiveCreditThree.cfg`, whose readers hold each group from the start). | A stream that could pause reads on and sheds, as on main, while a slow reader holds back the rest. |
| `ChargedAllowance` | Receive | A pause is charged no more than its slot's allowance (same configs). | The paused streams stop summing to under the cap, so a later one is refused. |
| `evict`'s assertion | Receive | Eviction never picks a stream whose reader may still progress (`Assert` in `EvictLoop`). | A reader that would have finished loses its group. |
| `Progress` and `Ends` | Receive | Every stream is read whole or evicted, and stopped (`AllEnd`). | A stream holds credit forever: the connection hangs. |
| `WithinCredit` | Send | Never more streams open than the peer granted. | The peer closes the connection with STREAM_LIMIT_ERROR. |
| `Progress` and `Ends` | Send | Every behavior ends quiet, and once quiet every group was sent and freed or given up, is a live group's stream, or waits only on live streams it may not reset. | A subscription stops sending behind a group that never opens. |
| `TopDelivered` | Send | Once quiet, each subscription's highest-ranked group reached the peer, was reset for a higher priority, or waits only on live streams it may not reset. | A waiting group holds back a newer one that could reset an unfinished stream: the stall #8607 fixed. |

TLC's own liveness check is about a thousand times slower here, so liveness is checked through safety. `Progress` is an action property: every step other than the final stutter raises a bounded measure, so every behavior ends stuttering in one state. `Ends` is an invariant: every state is done, or one of the system's steps is enabled there, which weak fairness takes. `Ends` reads each step's guard in place of ENABLED, and `Guarded` checks the two agree on a smaller config. The comment above `Lifecycle` in ReceiveCredit.tla gives the measure.

#9210's claim that every stream pauses, charged its slot's allowance, is `EveryPauseFits` and `ChargedAllowance`. It holds because a lease carries the window ingest shapes for the room its chunk leaves (`Lease::set_window`), and the hand back sets it under the same lock before queueing the grant (`HandBack`): set in a later lock hold, a driver on another thread could build the grant from the old window, and credit granted cannot be taken back.

Each model has witnesses (`NeverPaused`, `NeverEvicted`, `NeverStuckWaiting`, `NeverPartial`, `NeverPausedInside`; `NeverPreempts`, `NeverWaits`, `NeverGivesWay`, `NeverBlocked`). Each one is expected to fail, which shows TLC reaches the state. Check one after changing a step's guard.

## Run

```sh
quest gate bash sh/tla.sh rs/moq-net/verify/credit
quest gate just rs tla moq-net/verify/credit/ReceiveCredit.tla -config rs/moq-net/verify/credit/ReceiveCreditMain.cfg
```

| Config | Covers | States | Time |
| --- | --- | --- | --- |
| `ReceiveCredit.cfg` | #9210, two streams of two one-segment frames, a window two streams exhaust, readers late or leaving | 382k | 5 s |
| `ReceiveCreditMain.cfg` | main's design, as above | 363k | 7 s |
| `ReceiveCreditSegments.cfg` | #9210, frames of two segments, so a reader arriving mid-frame closes the gate there | 1.28M | 17 s |
| `ReceiveCreditWide.cfg` | #9210, a wider window, where the two slots' allowances differ; also the allowance rules | 143k | 3 s |
| `ReceiveCreditThree.cfg` | #9210, three streams, readers from the start; also the allowance rules | 1.62M | 30 s |
| `ReceiveCreditGuards.cfg` | `Guarded`, on one-unit frames | 10.3k | 8 min |
| `SendCredit.cfg` | two subscriptions, one newest first and one oldest first, one stream of credit | 89k | 6 s |
| `SendCreditPriority.cfg` | one subscription at a higher priority, so its opens preempt; also `Guarded` | 99k | 46 s |
| `SendCreditStreams.cfg` | two streams of credit, the orders swapped | 296k | 14 s |
| `SendCreditLive.cfg` | a middle group of each subscription never finishes | 41.6k | 3 s |

Every config passes. Nightly runs every config here and fails if a config with an `\* Open finding:` line passes.

The allowance rules are checked only where readers hold each group from the start (`Late = FALSE`), because of the gap below.

A gap is inherent, so it is no rule: a group with no reader, whose writer ran past the pace window, has no room (`Pace::room` is None). Its stream is granted a whole stream window. A reader arriving later closes the gate, and the pause is charged that credit, up to a stream window.

## Reading a failure

Follow Reading a failure in `rs/moq-net/verify/README.md`. Replay the path on the code:

- the moq-quic half with the stuck-window tests in `connection/streams/state.rs` (`stuck`, `lease_out_holds_eviction_off`, `evicted_taken_bytes_keep_credit_until_stop`);
- an ordering against the packet builder with `tests/sim.rs`;
- ingest's half with `Credited` in `coding/reader.rs` tests;
- the send half with the `Opener` tests in `transport/open.rs` and `TrackRun`'s tests in `lite/publisher.rs`.

In a receive trace, follow `pc`, `q[s]`'s `arr`, `taken`, `rel`, and `msd`, `charge`, and `slot` per stream.

## Coverage

Sync commit: `ef832d9bcc96e06e95a7e005f03f36c88ac6dc4c`; with `Shaped`, moq-dev/moq#9210 at `c0a70c8ab`. Run `git log ef832d9bcc.. -- rs/moq-quic/src/connection/streams rs/moq-net/src/coding/reader.rs rs/moq-net/src/transport/boxed.rs rs/moq-net/src/transport/open.rs rs/moq-net/src/model/pace.rs rs/moq-net/src/model/frame.rs rs/moq-net/src/lite/publisher.rs` to see what changed since. Follow Coverage in `rs/moq-net/verify/README.md` after a change to a covered function.

| Code | Model |
| --- | --- |
| `rs/moq-quic/src/connection/streams/state.rs` `StreamsState::received` | `Send` |
| `rs/moq-quic/src/connection/streams/state.rs` `StreamsState::write_control_frames`; `rs/moq-quic/src/connection/streams/recv.rs` `Recv::max_stream_data`, `Recv::can_send_flow_control`, `Recv::sent_max_stream_data` (#9210: `Recv::set_window`, `Recv::credit`) | `Transmit`, `Limit`, `CanGrant` (`Credit`) |
| `rs/moq-quic/src/connection/streams/lease.rs` `poll_lease`, `restore`, `release`, `queue_max_stream_data`, `Leased::release` (#9210: `Lease::set_window`) | `Lease`, `HandBack`, `Queue` (`Shape`) |
| `rs/moq-quic/src/connection/streams/lease.rs` `evict_if_stuck`, `trim_unlooked`; `rs/moq-quic/src/connection/streams/state.rs` `StreamsState::ranks`, `StreamsState::unlooked`, `StreamsState::stuck_waiting`, `StreamsState::evicted_held` | `Pass`, `EvictLoop`, `Evict`, `Unlooked`, `Order`, `stuckWaiting` |
| `rs/moq-net/src/coding/reader.rs` `Reader::poll_read_frame`, `Reader::poll_admit`, `Reader::poll_pace` | `Step`, `Park`, `Write` |
| `rs/moq-net/src/model/frame.rs` `Producer::poll_ready`, `Producer::unsealed`; `rs/moq-net/src/model/pace.rs` `window`, `Pace::paced`, `Pace::ahead`, `Pace::took`, `Pace::park` (#9210: `Pace::room`, `Producer::room`, `Producer::unsealed_after`) | `Paced`, `Unsealed`, `PaceWindow`, `Take` (`Room`) |
| `rs/moq-net/src/transport/boxed.rs` `Budgeted::window`, `Budgeted::pause`, `Budgeted::held`, `Budgeted::release`, `Shared::reserve` (#9210: `Pacing::cap`, `Pacing::allowance`, `Slots::take`, `Budgeted::shape`, `Budgeted::shaper`, `Shaper::window`) | `PaceWindow`, `Charge`, `charge`, `Stop` (`Cap`, `Allowance`, `LowestFree`, `Shape`) |
| `rs/moq-net/src/lite/publisher.rs` `TrackRun::opening`, `GroupServe::poll_open`, `TrackRun::stuck`, `TrackRun::give_way`, `TrackRun::shed`, `TrackRun::poll_step` | `PollOpen`, `Stuck`, `Admitted`, `Cursor`, `Step` |
| `rs/moq-net/src/transport/open.rs` `Rank::preempts`, `Line::join`, `Line::leave`, `Line::wake_head`, `Line::resets`, `Lines::make_room`; `rs/moq-net/src/priority.rs` `in_group` | `Rank`, `Above`, `Join`, `Leave`, `Preempts`, `MakeRoom`, `InGroup` |

What the model relies on in that code:

- A stopped stream returns its paused credit and frees its slot (`Stop`), so a reader that never returns cannot pin the paused budget.
- Eviction passes over a reader that may still progress, and an evicted reader's taken units keep their credit until it stops (`evicted_held`).
- Only the head of the line asks the transport, and leaving the line wakes the next head (`Line::leave`).
- A waiting group gives way to one that outranks it (`give_way`), and while it is `stuck` the cursor hands such a group out. Without either, `SendCreditLive.cfg` breaks `TopDelivered`, which the configs without a live group miss.

## Not covered

Both models leave out loss, gaps, and reordering within a stream: every unit arrives in order. They also leave out rate caps, shrink debt, frame headers' bytes, and the peer resetting a stream on the receive model. Every stream ranks at one priority on the receive side, so eviction order is newest first. Relay chains are left out, since pausing is per hop. So are the front's shedding past a pause and a group's expiry. The send side has two priority levels (`High` and the rest). TLC's simulate mode crashes on ReceiveCredit.tla (an `ArrayIndexOutOfBoundsException` in TLC), so larger shapes run exhaustively, as ReceiveCreditThree.cfg does. A pass covers two streams, or three with readers held from the start, of two or three frames, and two subscriptions of three groups, one of each live in `SendCreditLive.cfg`.
