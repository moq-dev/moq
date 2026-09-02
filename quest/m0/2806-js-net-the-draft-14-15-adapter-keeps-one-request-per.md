# [S] js/net: a duplicate PUBLISH_NAMESPACE on draft-14/15 strands the first request

## Goal

On draft-14/15, a second `PUBLISH_NAMESPACE` for a namespace that is already
announced is refused without disturbing the first: the first request's
`PUBLISH_NAMESPACE_DONE` still withdraws it, and a withdrawal the adapter
cannot resolve no longer ends the session.

## Plan

`ControlStreamAdapter` in `js/net/src/ietf/adapter.ts` keeps a namespace to
request-ID map for the draft-14/15 messages keyed by name rather than by
request ID. Both places that decode a `PUBLISH_NAMESPACE`
(`#parseAndRegisterNamespace` and the `0x06` route arm) do a plain `Map.set`,
so the duplicate overwrites the live mapping during decode, before the
subscriber gets to refuse it with 409. The first request's later DONE then
resolves to the refused request, finds no stream, and returns, leaving the
announcement up forever. `readNamespaceRequestId` also throws
`unknown namespace` for an unresolvable withdrawal, which is session-fatal, and
deletes from `#namespaces` without touching `#namespacesByRequestId`.

- First wins: register the mapping only when the namespace is not already
  live. The refused duplicate still gets its own `#namespacesByRequestId`
  entry so its 409 close can find it.
- On DONE or CANCEL, delete the namespace mapping only if it points at the
  request being closed, and keep both maps in step.
- A withdrawal that resolves to nothing is dropped, not fatal: the peer's
  duplicate was already refused, so there is nothing left to withdraw.
- Fold the two decode sites into one, since the bug was in the copy.
- Regression test through `ControlStreamAdapter` (not `NativeSession`, which
  bypasses it): two PUBLISH_NAMESPACE for one namespace, the second refused,
  then DONE for the first withdraws it. Cover the other two fixes too: a
  withdrawal that resolves to nothing leaves the session open, and a CANCEL
  leaves `#namespaces` and `#namespacesByRequestId` in step.

Only reachable on draft-14/15 against a peer that sends the duplicate; draft-19
is negotiated by default.

## Closes

- [#2806](https://github.com/moq-dev/moq/issues/2806) - close this issue when the quest finishes
