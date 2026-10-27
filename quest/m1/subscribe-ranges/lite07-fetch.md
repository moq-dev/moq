# [M] Remove FETCH from lite-07

## Goal

Lite-07 has no FETCH, in Rust, JS, and the draft. Rust and JS stop sending
and serving lite-07 FETCH, and read past groups through range subscriptions
instead. Published versions (lite-03..06) keep FETCH.

## Plan

Decided 2026-10-25 (maintainer, on
[moq-dev/moq#6807](https://github.com/moq-dev/moq/pull/6807)): the removal
lands in Rust and JS together, once both fill through the subscription, since
JS still requests and serves wire FETCH on lite-07
(`track.Consumer.fetchGroup`) and a Rust-only removal would drop a current
cross-language capability.

Decided 2026-10-27 (maintainer): split out of
[JS ranges](/quest/m1/subscribe-ranges/js.md), so the rest of JS ranges
starts now instead of waiting on
[Lite-07 ranges](/quest/m1/subscribe-ranges/lite.md). This quest waits on
both.

- `drafts/draft-lcurley-moq-lite.md` drops FETCH from lite-07, with its
  changelog bullets (FETCH max age and epoch). Run `just drafts check`.
- Lite-07 has the only `NotFetchable` code; check whether it still has a
  sender once FETCH is gone.
- Run `just test interop --all`.

Public API: none expected. Wire: lite-07 (unpublished) loses FETCH.

## Required

- [Lite-07 ranges](/quest/m1/subscribe-ranges/lite.md) - Rust fills lite-07 through the subscription
- [JS ranges](/quest/m1/subscribe-ranges/js.md) - JS reads and serves ranges, so it no longer needs lite-07 FETCH
