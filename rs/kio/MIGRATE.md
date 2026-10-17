# Migrating kio

How to update your code for each breaking change, newest first.

## Unreleased

### `Pollable` merged into `Task`, and unused surface removed (#4156)

`Fan`, `Hold`, `WeakLock`, `Pollable`, and the public `Lock::downgrade` are gone.
`Task` gained an `Output` type and has a blanket impl for `FnMut(&Waiter) -> Poll<R>`.
`poll_used` and `poll_unused` return `Poll<Result<(), Closed>>` instead of `Poll<Option<()>>`.
`Waiter::context()` replaces `Context::from_waker(waiter.waker())`.
Match `Poll::Ready(Err(kio::Closed))` where you matched `Poll::Ready(None)`.
