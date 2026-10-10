# [M] Origin read API

## Goal

`@moq/net`'s origin has one way to wait for a broadcast and follow it across
publisher restarts: `request(path, { announced: true })`. Its `active`
becomes a fresh consumer on a restart, `undefined` on an end, and resolves
again on the next start. `follow` and `broadcasts` are deleted, and
@moq/watch drops its follow-and-re-request loop.

## Plan

Decided 2026-10-10 in the [lazy discovery](/quest/m1/lazy-discovery/README.md)
interview. Lazy discovery needs every announce waiter to hold a closeable
watch, and these two APIs either duplicate `request` or cannot be closed.

Facts (2026-10-10, `origin/main` e5446b036):

- `follow(path)` (`js/net/src/origin.ts:1641`) is an announcement stream
  reduced to the route serving `path`. Its only caller outside tests is
  @moq/watch, which pairs it with `request(name, { announced })` and
  re-requests on `restart` (`js/watch/src/broadcast.ts:284-310`), because a
  `Requesting` stays on the instance it resolved and errors once it stops.
- `broadcasts(scope)` (`origin.ts:1605`) is a synchronous live map with no
  close. Only tests, benches (`js/net/bench/broadcasts.ts`,
  `js/net/bench/forward.ts`), and `doc/lib/js/net.md` use it.

Decisions:

- Fold `follow` into `request(path, { announced: true })` as described in
  the Goal. Pinning a single instance stays available through `epoch`.
- Delete `broadcasts`; `announced(scope)` covers it.
- `request(path)` without `announced` keeps today's blind behavior.

Update `doc/lib/js/net.md`, the `js/net/examples`, and the benches. Tests: an
announced request swaps `active` on a restart, clears it on an end, and
resolves again on the next start; an `epoch`-pinned request still refuses
another instance; @moq/watch plays through a publisher restart.

Public API: `Origin.Consumer.follow` and `broadcasts` removed (also on
`Producer`); `request(..., { announced: true })` semantics change: `active`
swaps on a restart instead of the request erroring. External callers (the
customer is on 0.4.2) need a migration note in the PR description and
`doc/lib/js/net.md`, with before and after for the follow-and-re-request
pattern. Wire: none.
