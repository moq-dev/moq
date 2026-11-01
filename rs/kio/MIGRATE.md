# Migrating kio

How to update your code for each breaking change, newest first.

## Unreleased

### `async` methods moved behind the `async` feature

`Queue::{push, pop}`, `Producer::{wait, closed, wanted_changed}`, `Consumer::{wait,
closed}`, `Shared::wait`, `ConsumerWeak::closed`, `ProducerWeak::closed`,
`clock::drive`, and `clock::Interval::tick` now compile only with the new `async`
feature, which is off by default; `sim` enables it. Add `features = ["async"]` to
the `kio` dependency of each crate that `.await`s them. Each `poll_*` method is unchanged, and still drives the same wait without it.

### `Pollable` merged into `Task`, and unused surface removed (#4156)

`Fan`, `Hold`, `WeakLock`, `Pollable`, and the public `Lock::downgrade` are gone.
`Task` gained an `Output` type and has a blanket impl for `FnMut(&Waiter) -> Poll<R>`.
`poll_used` and `poll_unused` return `Poll<Result<(), Closed>>` instead of `Poll<Option<()>>`.
`Waiter::context()` replaces `Context::from_waker(waiter.waker())`.
Match `Poll::Ready(Err(kio::Closed))` where you matched `Poll::Ready(None)`.
