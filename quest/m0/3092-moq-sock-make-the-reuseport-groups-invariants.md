# [M] moq-sock: make the reuseport group's invariants unrepresentable, not documented

## Goal

Implement and verify the behavior tracked in [#3092](https://github.com/moq-dev/moq/issues/3092)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

`moq-sock` (added in #3078) exposes `shard::{Shard, bind, Lock, MAX_SHARDS}` as four independent public items, and correct use requires holding three rules in your head at once:

1. Acquire a `Lock` on a named port *before* the first member binds, and hold it until the group is gone.
2. Call `bind` for every member in index order, with nothing else in between.
3. Never resize the group.

None of them are enforced by a type. Rule 1 is the sharpest: `bind`'s probe only refuses a group that is already bound, so two same-UID processes constructing at once each pass the probe before either holds the port, then interleave into one reuseport group whose positional BPF filter routes one process's connection ids to the other's sockets. That is silent, and it happens exactly during a rolling restart.

Not a live bug today: both in-tree callers (`moq_tokio::worker::Workers::new` and `moq_relay::uring::Workers::bind`) acquire the lock first. #3078 documents the precondition on `bind` so an outside caller can at least read it. But the invariant used to be crate-private and is now prose, which is the shape [CLAUDE.md](https://github.com/moq-dev/moq/blob/dev/CLAUDE.md) argues against ("make misuse unrepresentable rather than merely documented").

##### Shape worth considering

A `shard::Group` that owns the lock, validates the count once, and hands out members in order:

```rust
let mut group = shard::Group::acquire(addr, count)?;  // holds Option<Lock>
let (shard, socket) = group.bind()?;                  // index order, enforced
```

That subsumes the count bound, the ordering rule, and the lock lifetime, and it deletes duplication that already exists in both callers (the `count > MAX_SHARDS` check, the `Lock::acquire` dance, and `Shard::new(..).expect(..)`).

##### Why it is not a small change

`moq-tokio` never calls `shard::bind` from its construction loop. It calls `Worker::spawn`, and the bind happens on the worker thread inside the quinn and noq backends. A `Group` that returns sockets would mean moving where moq-tokio creates its sockets, across both backends. That deserves its own PR and its own testing rather than riding along with the extraction.

`moq-sock` is at 0.0.1 and is not being published for a while, so the reshape stays cheap until then.

Found by Codex reviewing #3078 (P1, `rs/moq-sock/src/shard.rs`); verified against the code and deliberately deferred.

## Closes

- [#3092](https://github.com/moq-dev/moq/issues/3092) - close this issue when the quest finishes
