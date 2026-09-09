# [M] QUIC workers: dropping one split() Server resizes the reuseport group

## Goal

`moq_tokio::worker::Workers` can no longer lose one reuseport socket while
its siblings keep serving: `split(self)` consumes the group and returns a
handle that owns every socket, each `Server` shares it, and the group stops
when any member's `run()` future completes.

## Plan

Found in the second review round on #2921. The worker group is off by default
and Linux-only, so this is not urgent, but it is the last unenforced part of
the "bound once, in order, never resized" invariant that connection-ID
steering rests on.

### Mechanism

`Workers::split` (rs/moq-tokio/src/worker/group.rs:193) returns
`Vec<(Server, Spawner<'_>)>`. The `Spawner` borrows the group, so a caller
cannot drop a worker's *thread* on its own. But `Server` owns the
`quinn::Endpoint`, and therefore the socket, and `Server::listen(mut self)`
consumes it, so the accept loop has to own it. That leaves two ways to take
one socket out of the group:

1. Drop a returned `Server` without running it.
2. Let the future built from one `Server` return while its siblings keep
   serving.

Either way Linux moves the last socket in the reuseport array into the
vacated slot. The cBPF filter still reduces modulo the original count, and
connection IDs encoding the moved member now select an index past the end of
the array, so the kernel falls back to hashing the 4-tuple. Live sessions on
a worker that never failed get misrouted. The `split` docs (group.rs:185-192)
say so and point here.

`moq-relay` already does the right thing: `Relay::run` ends everything on the
first worker to finish (rs/moq-relay/src/relay.rs:359-385) and then calls
`Workers::shutdown` after the select (:428-448). An embedder gets no such
guarantee.

### Design

`Spawner::run` already takes a `FnOnce() -> Future` builder (group.rs:289),
so the builder shape is settled. What changes is ownership:

- `split(self)` consumes `Workers` and returns a group handle that owns every
  socket. No socket can be dropped alone because no caller holds one.
- Each `Server` holds a clone of that handle, so running or dropping a
  `Server` never closes its socket; the group does, all at once.
- The group stops when any member's `run()` future completes, matching what
  `Relay::run` does by hand today.
- No callback parameters. The builder passed to `run` stays a builder.

Regression: a test that drops one `Server` and lets the others serve, then
proves the socket count and the steering filter are unchanged and a session
on a surviving worker keeps its route.

## Required

- [Reuseport group formation](/quest/m2/reuseport-group.md) - the
  `moq_sock::shard::Group` side of the same invariant; the group handle here
  is built on a group that is complete before it serves

## Closes

- [#2964](https://github.com/moq-dev/moq/issues/2964) - close this issue when the quest finishes
