# [M] One lifecycle vocabulary

## Goal

Every model handle in moq-net and `@moq/net` (broadcast, track, group, their
demand, listings) uses each lifecycle word with one meaning, and Rust and JS
agree on when each observer fires.

## Plan

Decided 2026-10-23 (API audit follow-up, maintainer): `finish()` declares a
clean end, `abort(err)` is terminal, `closed()` resolves on the final outcome,
and JS `close()` releases only that handle, like a Rust drop.
[moq-dev/moq#6750](https://github.com/moq-dev/moq/pull/6750) landed the
producer verbs, `closed()` semantics in both languages, group consumer
`close()`, `Lister.closed`, `Demand.unused`, and the reader `end()` removal.

What remains is JS consumers whose `close()` still ends state other handles
share. The JS wire layer and origin rely on it today:

- `Announce.Consumer.close(abort?)` closes the shared announcement queue, which
  is how a reader cancels the stream. Pick: count consumers and give the
  producer an `unused()` the session watches (recommended), or keep one reader
  per queue and document `close()` as cancel.
- The last `Broadcast.Consumer.close()` closes the broadcast, including a
  producer's. The origin relies on it: superseding a published broadcast
  closes its producer once no consumer holds it. Pick: the producer stays open
  and publishers watch `demand().unused()`, as in Rust (recommended), or keep
  it for wire-owned broadcasts only and close superseded producers explicitly.

`@moq/e2ee` wraps the old verbs (`Group.Producer.close(abort?)`); mirror the
net split there.

Public API: `@moq/net` breaking (consumer `close()` semantics), plus
`@moq/e2ee`. Wire: none.
