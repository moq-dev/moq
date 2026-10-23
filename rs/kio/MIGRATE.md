# Migrating kio

How to update your code for each breaking change, newest first.

## Unreleased

### Simulator instrumentation is private ([moq-dev/moq#6529](https://github.com/moq-dev/moq/pull/6529))

The test-only `sim` feature is `_sim`. Production consumers should remove it;
moq-clock's simulator enables the internal hook itself.

### `Pollable` merged into `Task`, and unused surface removed (#4156)

`Fan`, `Hold`, `WeakLock`, `Pollable`, and the public `Lock::downgrade` are gone.
`Task` gained an `Output` type and has a blanket impl for `FnMut(&Waiter) -> Poll<R>`.
`poll_used` and `poll_unused` return `Poll<Result<(), Closed>>` instead of `Poll<Option<()>>`.
`Waiter::context()` replaces `Context::from_waker(waiter.waker())`.
Match `Poll::Ready(Err(kio::Closed))` where you matched `Poll::Ready(None)`.
