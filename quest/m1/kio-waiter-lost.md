# [XS] kio waiter overflow stays idempotent

## Goal

A `kio::Waiter` that has registered on more than 8 lists still registers for
free on each list it recorded, so a retained standalone `Waiter` polled
repeatedly no longer appends a duplicate entry to those lists every time.

## Plan

`Waiter::record` in `rs/kio/src/waiter.rs` returns early once `lost` is set,
before it matches the list's tag against the recorded slots. `Park` retires
such a waiter, so it never notices, but `Waiter` is public and a caller that
keeps one across polls grows every list it sits on, one live entry per
register until the list drains, and gets a duplicate wake for each.

Match the recorded tags first and only then fall back on `lost`. This was
written with the regression test `overflow_keeps_recorded_lists_idempotent`
during https://github.com/moq-dev/moq/pull/4240, which squash-merged before
it was pushed, so it needs rewriting. Keep the recorded-tag probe the tight
common-case loop it is today; `rs/kio/benches/waiter.rs` shows whether it
moved.

A list past the 8 slots is unrecorded and still has to append on every
register, which is the pre-#4240 behavior. Say so on `Waiter::register` so a
standalone caller knows the bound, rather than growing the record array.
