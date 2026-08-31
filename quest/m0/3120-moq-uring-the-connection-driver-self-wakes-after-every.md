# [M] moq-uring: the connection driver self-wakes after every GSO train, re-walking every ready stream…

## Goal

Implement and verify the behavior tracked in [#3120](https://github.com/moq-dev/moq/issues/3120)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

Profiling the io\_uring relay under load (`dev` @ `fc57e0175`, `perf record -F 499` on the relay process only, steady state), `kio::waiter::WaiterList::register` is the single hottest symbol in both workloads, at roughly 2-3x its share on the tokio worker path:

| symbol | video, io\_uring | video, tokio workers | chat, io\_uring |
|---|---|---|---|
| `kio::waiter::WaiterList::register` | **4.59%** | 2.22% | **6.73%** |
| `moq_uring::quic::connection::Driver::poll` | 3.02% | - | 2.86% |
| `kio::task::Tasks<T>::poll` | 1.50% + 1.09% | 1.72% + 1.11% | 2.76% + 1.23% |
| `SlotWaker::wake_by_ref` | 1.28% | 0.75% | 1.24% |

(video = 1:200 fan-out, 60fps, 4 KB frames; chat = 1:500 fan-out, 10 msg/s, one group per message; `--runtime-workers 4`.)

#### Mechanism

`Connection::flush` ends every successful send by waking itself:

```rust
if let Err(err) = tx.send(filled, to, SEGMENT) {
    return Poll::Ready(Error::Io(err.to_string()));
}
// Requeue behind the other ready tasks. If quiche is drained, the next
// poll costs one empty acquire and then parks normally.
waiter.waker().wake_by_ref();
Poll::Pending
```

The self-wake is deliberate (it yields the transmit pool to other connections between trains), but it re-runs the *whole* of `Driver::poll`, not just the send. Each of those turns walks quiche's readiness iterators and touches a map entry per ready stream:

```rust
for id in conn.readable() {
    queue_accept(&mut state, id);
    if let Some(mut waiters) = state.readable.remove(&id) {
        waiters.wake();
    }
}
for id in conn.writable() {
    if let Some(mut waiters) = state.writable.remove(&id) {
        waiters.wake();
    }
}
```

Waking removes the entry, so every still-interested stream task re-registers on its next poll. In a fan-out most streams are writable most of the time, so the cost is O(ready streams) of map-remove plus waiter register/wake **per GSO train**, where it should be per ingress event. That is the amplifier behind both the `WaiterList::register` share above and the SipHash cost filed separately.

`state.finishing.retain(...)` compounds it: it calls `conn.stream_capacity(*id)` for every finishing stream on every one of those turns, and allocates a fresh `Vec` (`let mut collected = Vec::new()`) each time.

#### Impact

CPU per delivered frame is at parity with the tokio worker path at low connection counts and regresses as connections grow, which is the shape you would expect if per-turn work scales with streams per connection:

| connections | tokio workers | io\_uring |
|---|---|---|
| 401 | 59.77 us/frame | 59.41 us/frame |
| 801 | 58.80 us/frame | **66.56 us/frame** |
| 1601 | 52.81 us/frame | **58.72 us/frame** |

#### Suggestion

Keep the fairness yield but stop paying for a full driver poll to get it: separate "there is more to send" from "readiness changed", so a requeue for the transmit pool does not re-walk `readable()`/`writable()`. Failing that, only wake the waiters whose readiness actually changed since the last turn rather than removing and re-registering every ready stream.

## Closes

- [#3120](https://github.com/moq-dev/moq/issues/3120) - close this issue when the quest finishes
