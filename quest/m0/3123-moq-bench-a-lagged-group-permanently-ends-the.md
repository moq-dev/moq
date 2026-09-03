# [M] moq-bench: a lagged group permanently ends the subscription, so offered load silently decays mid-run

## Goal

Implement and verify the behavior tracked in [#3123](https://github.com/moq-dev/moq/issues/3123)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

`moq-bench` loses subscriptions permanently over the course of a run, so the load it offers decays and the numbers it reports are not the numbers the config asked for. Worse, the decay is load-dependent: a relay that struggles sheds more subscribers, which makes it look better on every per-connection metric.

Observed on a 1:200 video fan-out (`--fanout v --connections 201 --fps 60 --frame-size 4000 --group-size 59`) against `dev` @ `fc57e0175`: the `subscriptions` gauge falls from 200 to ~150 within a minute, and delivered groups/s settle well under the configured rate:

| relay mode | groups/s (200 expected) |
|---|---|
| tokio, shared runtime | 155.6 |
| tokio, 4 workers | 193.5 |
| io\_uring, 4 workers | 199.0 |

The relay logs `no route can serve the rest of this group err=cancelled` and the generator logs `subscribe canceled (idle)` alongside it.

#### Mechanism

The relay signals a subscriber that fell behind by failing the group with `Error::Lagged` (`rs/moq-net/src/model/resume.rs`, `give_up`). In `rs/moq-bench/src/connection.rs`, `drain` propagates that out of both loops with `?`:

```rust
while let Some(mut group) = track.recv_group().await? {
    gaps.observe(group.sequence);
    let mut first = true;
    while let Some(frame) = group.read_frame().await? {
```

The inner `?` is the problem: a mid-group `Lagged` ends the whole `drain`. `spawn_drain` then just logs it at debug and lets the task finish:

```rust
tasks.spawn(async move {
    if let Err(err) = drain(broadcast, &stats).await {
        tracing::debug!(%path, %err, "subscription ended");
    }
});
```

Nothing re-subscribes, so that connection is out of the run for good. A real player skips a lagged group and keeps watching; it does not hang up.

#### Fix

Treating a group-level error as the end of *that group* rather than of the subscription is enough to hold the load steady. I ran the matrix with this applied and the group counts held at the configured rate (1600.1 of 1600 groups/s at 1601 connections, versus 1583 before):

```rust
let mut first = true;
// A live subscriber skips a group it fell behind on; it does not hang up. The
// relay reports that as `Lagged` mid-group, so treat any group-level error as
// the end of *this* group and keep the subscription for the next one.
while let Some(frame) = match group.read_frame().await {
    Ok(frame) => frame,
    Err(err) => {
        tracing::debug!(sequence = group.sequence, %err, "group ended early");
        None
    }
} {
```

The outer `recv_group()` error is more genuinely terminal (the track or session is gone), though re-establishing the subscription there would make long runs more honest too. Happy to open a PR for the inner-loop change if that shape looks right.

Until this is fixed, any `moq-bench` comparison of two relay builds should be read with the caveat that throughput differences partly reflect how many subscribers each build kept, not how much each could carry.

## Closes

- [#3123](https://github.com/moq-dev/moq/issues/3123) - close this issue when the quest finishes
