# [S] A departed subscription releases its streams

## Goal

On lite-07, when a subscription's last reader leaves, every stream it held
upstream is released, so the credit returns for the next subscription. A
relay test with exactly 3 bidirectional stream slots serves a second
subscription after the first leaves, as it already does on moq-transport-22
and on lite-07 with 4 slots.

## Plan

Found 2026-11-04 by the scheduler's relay test
(`a_subscription_past_the_credit_is_served_once_credit_returns`, from
[moq-dev/moq#7894](https://github.com/moq-dev/moq/pull/7894)): on lite-07 with exactly 3 bidirectional slots, a second
subscription is never served after the first leaves, while 4 slots work and
moq-transport-22 works with 3. The likely cause is a stream the departed
subscription keeps open upstream (its request stream, or a companion such as
the Tracks stream) after its reader leaves. Reproduce it with the 3-slot
relay test first, find which stream stays open and on which side, fix it, and
set that relay test's credits back to the tight case. With opens now waiting
for credit instead of failing, a leaked stream stalls later subscriptions
silently, and low-credit peers such as browsers hit it first.


### Investigation

2026-11-04: reproduced before any fix with the existing relay regression, changing
only lite-07's credit from 4 to exactly 3. The command
`just rs test -p moq-net --test integration -E 'test(a_subscription_past_the_credit_is_served_once_credit_returns)'`
failed: "moq-lite-07-wip: the waiting subscription never got credit".
The client stream types before departure were `[7, 1, 6]`: AUTH, ANNOUNCE, TRACK.
The first SUBSCRIBE had not reached the publisher. The initial diagnosis of a
leak after an established subscription was therefore incomplete.

`ServeLoop::poll` waits in `ServeMode::Establish` without checking whether demand
left, keeping the answered `HeldTrack` open. An experimental abandonment check
and explicit cancellation of the opening registration released that TRACK and let
the second TRACK resolve. With the first publisher track kept alive, the same
regression then failed later: "no group after credit returned". The second
SUBSCRIBE was now blocked behind its own TRACK. This confirms that departure
cleanup alone does not achieve the Goal. The incomplete implementation and test
edits were removed pending the contract decision above.

Before claiming, checked the current patches of
[moq-dev/moq#7670](https://github.com/moq-dev/moq/pull/7670) and
[moq-dev/moq#7737](https://github.com/moq-dev/moq/pull/7737). Their stale-group and
pending-join changes touch the files but do not implement this cleanup or remove
the TRACK/SUBSCRIBE credit dependency.


## Waiting

- Maintainer: choose the stream-budget contract before implementation. Investigation
  on main `0834a1eab0` found that three slots cannot establish even the first media
  subscription: AUTH and ANNOUNCE hold two, TRACK holds the third until SUBSCRIBE
  answers, and SUBSCRIBE needs a fourth. Options:
  1. **(Recommended)** Make lite-07 subscription setup fit three slots by returning
     immutable track metadata on SUBSCRIBE, removing the companion TRACK from this
     path. This revises the unpublished wire and needs Rust/JS and draft updates;
     retain standalone TRACK for info-only queries.
  2. Keep the separate TRACK/SUBSCRIBE handshake and scope this quest to departure
     cleanup with four slots, documenting its minimum credit requirement. This
     abandons the present three-slot Goal.
  3. Release TRACK before SUBSCRIBE opens when credit is exhausted. This creates a
     demand gap for on-demand producers and needs an explicit decision about their
     lifetime; do not silently weaken the current hold guarantee.
  Check: record the chosen contract in Plan, then resume implementation.
