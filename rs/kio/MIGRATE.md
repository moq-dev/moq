# Migrating kio

How to update your code for each breaking change, newest first.

## Unreleased

### `Pollable` merged into `Task`, and unused surface removed (#4156)

`Fan`, `Hold`, `WeakLock`, `Pollable`, and the public `Lock::downgrade` are gone.
`Task` gained an `Output` type and has a blanket impl for `FnMut(&Waiter) -> Poll<R>`.
`wait(f)` is `Pending::new(f)`.
`poll_used` and `poll_unused` return `Poll<Result<(), Closed>>` instead of `Poll<Option<()>>`.
`Waiter::context()` replaces `Context::from_waker(waiter.waker())`.

```rust
// Before
let value = kio::wait(|waiter: &kio::Waiter| state.poll(waiter)).await;
match producer.poll_unused(waiter) {
	Poll::Ready(None) => {} // closed
	_ => {}
}
// After
let value = kio::Pending::new(|waiter: &kio::Waiter| state.poll(waiter)).await;
match producer.poll_unused(waiter) {
	Poll::Ready(Err(kio::Closed)) => {}
	_ => {}
}
```
