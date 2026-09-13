# [L] An owning split worker group

## Goal

An embedder cannot drop one returned worker server and remove its socket
while its siblings continue serving. The published `moq_tokio::worker`
ownership change is functional before the dev release.

## Plan

`Workers::split(&mut self)` currently returns `Vec<(Server, Spawner<'_>)>`.
The spawners borrow the thread owner, but each returned server owns its
endpoint and socket. Dropping a server can therefore resize the reuseport
array while its steering filter and surviving connection IDs remain unchanged.

Use the ownership direction already chosen for #2964: `split(self)` consumes
the bound workers and returns an owning group. Server handles retain the
group's socket ownership; individual handles cannot close a member socket.
The group owns termination and thread joins, and completion, cancellation,
or failure of a serving member ends serving for the group. Preserve the
existing once-only builder used to construct a worker-local future.

Make this ownership real in the same PR, using the current bind path, which
already finishes binding every member before returning. Retain all socket
owners until group serving has stopped, and preserve shutdown and Drop cleanup
without self-joining a worker thread. Do not merely change the signature and
leave callers responsible for the old lifetime invariant.

Migrate the relay and external examples to the owning group. Coordinate with
the relay embedding owner, but keep worker ownership independently usable by
`moq-tokio` callers. Preserve explicit shutdown where asynchronous joining is
needed and report serving failure through the owner.

An external-crate regression drops an unused server handle and proves the
remaining bound group does not lose a socket. Another completes a serving
member and proves the siblings stop and threads join. Cover cancellation,
panic, explicit shutdown, and dropping the owner with work in flight in Linux
CI, at least nightly. Existing worker serving tests must still pass.

Public API: breaking ownership and return shape of `Workers::split` and the
associated serving handles. Wire: no format change. Hardened partial formation
in the `moq-sock` 0.0.x layer and its integration remain in M2; no M2 quest or
dev-merge gate blocks this API change.

## Related

- [Worker group integration](/quest/m2/2964-quic-workers-dropping-one-split-server-resizes-the.md) - adopts the hardened socket-group primitive without another public ownership change
