# [S] Listed tracks get their own session cap

## Goal

A session bounds the tracks its peers' TRACKS listings make it hold with its
own `session::Limits` field, apart from `announces`, so a heavy lister such
as a stats aggregator listing every node's bucket tracks can be sized
without also widening how many broadcasts a peer may announce, and the
`announces` doc stops undercounting what it bounds. Not here: a prefix or
name filter on the TRACKS message, or any other wire change.

## Plan

From the moq.pro stats and billing audit
([moq-dev/moq.pro#4513](https://github.com/moq-dev/moq.pro/pull/4513)
follow-ups). Checked against `main` at `b97c58535f`: every TRACKS_ADD takes
one slot from the receiving session's `announces` `Slots` and releases it on
TRACKS_REMOVE, TRACKS_FINISH, or stream end
(`rs/moq-net/src/lite/subscriber.rs` around `:2875`,
`js/net/src/lite/subscriber.ts` around `:1288`). Past the cap the receiver
closes its own session with `TooManyRequests`
(`rs/moq-net/src/session.rs` `Slots::acquire`). The audit's claim that each
TRACKS request takes a slot is wrong: the cost is per listed track.

Decided 2026-11-26 (maintainer, /quest-plan of the moq.pro#4513
follow-ups): ✅ a separate cap with no wire change (rejected: also a
track-name prefix field on TRACKS, since no consumer needs a subset and
moq-dash reads every bucket; only fixing the `announces` doc; dropping it).

- Add `Limits::tracks` in `moq-net` and the matching field in
  `js/net/src/util/limits.ts` (internal there), and draw listing slots from
  it. Default it to the `announces` default (100,000).
- moq-relay gains `limits.client.tracks` and `limits.peer.tracks` beside the
  `announces` flags in `rs/moq-relay/src/limits.rs`, defaulting to the
  matching `announces` value.
- `moq-ffi` exposes no `Limits` today, so no binding changes.

Public API: one new field on the `#[non_exhaustive]` `session::Limits`, two
new relay flags. Behavior: listed tracks no longer count against
`announces`. Wire: none.
