# [M] QUIC workers: dropping one split() Server resizes the reuseport group

## Goal

Implement and verify the behavior tracked in [#2964](https://github.com/moq-dev/moq/issues/2964)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

One assumption has already moved: `Spawner::run` now takes a
`FnOnce() -> Future` builder, so the issue context's objection that a builder
is a banned callback no longer holds. What is left is who owns the servers,
which is the first option below.

### Issue context

Found in the second review round on #2921. The worker group is off by default and Linux-only, so this is not urgent, but it is the last unenforced part of the "bound once, in order, never resized" invariant that connection-ID steering rests on.

#### Mechanism

`moq_tokio::worker::Workers::split` returns `Vec<(Server, Spawner<'_>)>`. The `Spawner` borrows the group, so a caller cannot drop a worker's *thread* on its own. But `Server` owns the `quinn::Endpoint`, and therefore the socket, and `Server::listen(mut self)` consumes it  -  so the accept loop has to own it.

That leaves two ways to take one socket out of the group:

1. Drop a returned `Server` without running it.
2. Let the future built from one `Server` return while its siblings keep serving.

Either way Linux moves the last socket in the reuseport array into the vacated slot. The cBPF filter still reduces modulo the original count, and connection IDs encoding the moved member now select an index past the end of the array, so the kernel falls back to hashing the 4-tuple. Live sessions on a worker that never failed get misrouted.

`moq-relay` does the right thing today  -  `Relay::run` ends on the first worker to finish and then calls `Workers::shutdown`  -  so its exposure is a shutdown that was already happening. An embedder gets no such guarantee.

#### Why it is not fixed in #2921

Making it unrepresentable means `Workers` owns the servers and drives them, so the caller passes something like `FnOnce(Server) -> impl Future` for the group to build each accept loop from. That is a callback parameter, which [CLAUDE.md](/CLAUDE.md) rules out of public APIs, and avoiding it is why the split/spawner shape exists at all. Worth a deliberate decision rather than a drive-by.

#### Options

- Accept a consumed `FnOnce(Server) -> Future` as a *builder* rather than a policy hook (no `Send + Sync + 'static` smuggling, no hidden timing), and have `run` wrap the future so the group stops when any member's future completes.
- Leave the contract documented (where it is now) and rely on callers.
- Fold it into the `SK_REUSEPORT` + `BPF_MAP_TYPE_REUSEPORT_SOCKARRAY` work from #2875. A map-based selector picks by slot rather than by position, so a member leaving stops being catastrophic and this whole class of problem goes away. Same place #2960 points.

Related: #2960 (a restarting relay joining the old process's group), #2875 (the epic).

## Closes

- [#2964](https://github.com/moq-dev/moq/issues/2964) - close this issue when the quest finishes
