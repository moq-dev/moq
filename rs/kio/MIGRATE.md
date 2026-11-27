# Migrating kio

How to update your code for each breaking change, newest first.

## Unreleased

Everything below breaks code written against `0.6.4`, the last release.

### `Wall::MOQ_EPOCH` removed and `MOQ_SIM_SEED` renamed ([kixel-ai/kio#22](https://github.com/kixel-ai/kio/pull/22))

kio no longer names MoQ.

`Wall::MOQ_EPOCH` is gone (it never shipped in a release: only a Git revision of
`moq-dev/moq` newer than `0.6.4` has it), and `Wall::from_unix_secs(secs)` is now `pub const fn`.
Define the epoch once in the lowest crate that needs it and use that everywhere:

```rust
pub const EPOCH: kio::clock::Wall = kio::clock::Wall::from_unix_secs(1_577_836_800); // 2020-01-01T00:00:00Z
```

`from_unix_secs` panics, which fails the build in a `const`, if `secs` overflows `u64`
nanoseconds.

The simulator reads its replay seed from `KIO_SIM_SEED` instead of `MOQ_SIM_SEED`, with no
fallback to the old name: a script that still sets `MOQ_SIM_SEED` silently runs on a
random seed. A failed `sim::run` prints `KIO_SIM_SEED=<seed>` in its replay hint.

In `moq-dev/moq`, after bumping the kio rev:

- Replace `Wall::MOQ_EPOCH` in `rs/hang` (`snapshot.rs`, `catalog/clock.rs`,
  `catalog/root.rs`), `rs/moq-mux` (`clock.rs`, `catalog/producer.rs`), `rs/moq-gst`
  (`sink/imp.rs`), `rs/moq-hls` (`export/mod.rs`), `rs/moq-net` (`model/clock.rs`),
  `rs/moq-rtsp` (`client.rs`), and `rs/moq-stats` (`produce.rs`), with the shared
  `EPOCH` above. A code search found these files; grep `MOQ_EPOCH` for the full list.
- Rename `MOQ_SIM_SEED` to `KIO_SIM_SEED` in `doc/lib/rs/kio.md` and in every script,
  workflow, and `just` recipe that sets it.

### `Hold` holds every wake on the thread ([moq-dev/moq#8293](https://github.com/moq-dev/moq/pull/8293))

`kio::hold()` returns a `Hold` that holds back every wake this thread makes through a
`WaiterList` until it drops; holds nest, and the outermost delivers. It takes no
generic parameter and is neither `Send` nor `Sync`. It replaces `Fan::hold()` and
`Hold<T>`: open a hold before taking the locks whose guards wake waiters, and let it
drop after them.

The scope changed with it. `Fan::hold()` deferred one fan's wakes made from any
thread; `kio::hold()` defers every list's wakes, but only those made on the thread
that opened it. A wake from another thread now lands at once, so a task it wakes may
find a lock still held: open a hold on every thread that wakes waiters under a lock.
A hold on the waking thread doesn't cover a lock another thread owns: once it drops,
the woken task can run while that owner still holds the lock, and nothing restores
the old cross-thread deferral. The lock's owner must open the hold around its own
wakes, or release the lock before another thread wakes the waiters that take it.

### `async` methods moved behind the `async` feature ([moq-dev/moq#7506](https://github.com/moq-dev/moq/pull/7506))

`Queue::{push, pop}`, `Producer::{wait, closed, wanted_changed}`, `Consumer::{wait,
closed}`, `Shared::wait`, `ConsumerWeak::closed`, `ProducerWeak::closed`,
`clock::drive`, and `clock::Interval::tick` now compile only with the new `async`
feature, which is off by default; `sim` enables it. Add `features = ["async"]` to
the `kio` dependency of each crate that `.await`s them. Each `poll_*` method is
unchanged, and still drives the same wait without it.

### Used and unused renamed to wanted ([moq-dev/moq#7036](https://github.com/moq-dev/moq/pull/7036))

On `Producer` and `ProducerWeak`:

- `is_used()` is now `wanted()`.
- `Producer::write_unused()` is now `write_unwanted()`, and the `Unused` it returned
  is now `Unwanted`: `Unused::Used` becomes `Unwanted::Wanted`, while `Idle` and
  `Closed` keep their names.
- `used()`, `unused()`, `poll_used(waiter)`, and `poll_unused(waiter)` are replaced by
  `poll_wanted_changed(waiter, last)`, which resolves to the new `wanted()` once it
  differs from `last`, or `Err(Closed)`. `Producer::wanted_changed(last)` awaits it
  with the `async` feature; `ProducerWeak` has no async form, so await
  `kio::wait(|waiter| weak.poll_wanted_changed(waiter, last))`. Wait for a consumer
  with `last = false`, and for the last one to go with `last = true`.

### `MaybeSend` removed ([moq-dev/moq#7475](https://github.com/moq-dev/moq/pull/7475))

kio no longer builds for WASM, so the `MaybeSend` alias is gone. Bound on `Send`.

### `Pollable` merged into `Task`, and unused surface removed ([moq-dev/moq#4156](https://github.com/moq-dev/moq/pull/4156))

`Fan` with its `Hold<T>`, `WeakLock`, `Pollable`, and the public `Lock::downgrade` are
gone.
`Task` gained an `Output` type and has a blanket impl for `FnMut(&Waiter) -> Poll<R>`.
A `Pollable` impl becomes a `Task` impl whose `poll` takes `&mut self` instead of `&self`.
`poll_used` and `poll_unused` return `Poll<Result<(), Closed>>` instead of
`Poll<Option<()>>`, before their rename above.
`Waiter::context()` replaces `Context::from_waker(waiter.waker())`.
Match `Poll::Ready(Err(kio::Closed))` where you matched `Poll::Ready(None)`.
