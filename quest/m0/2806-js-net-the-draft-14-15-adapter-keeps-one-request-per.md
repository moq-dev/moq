# [M] js/net: the draft-14/15 adapter keeps one request per namespace, so a duplicate strands the first

## Goal

Implement and verify the behavior tracked in [#2806](https://github.com/moq-dev/moq/issues/2806)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

`ControlStreamAdapter` keeps a namespace to request-ID map for the draft-14/15 messages that are keyed by name rather than by request ID ([`js/net/src/ietf/adapter.ts`](https://github.com/moq-dev/moq/blob/main/js/net/src/ietf/adapter.ts), `#namespaces`). It is written while decoding a `PUBLISH_NAMESPACE`, and a second request for the same namespace overwrites the first:

```ts
const namespace = await Namespace.decode(r);
this.#namespaces.set(namespace, requestId);   // overwrites
```

The overwrite happens during decode, before the message is dispatched, so nothing at the subscriber layer can prevent it. `Subscriber.runPublishNamespace` does refuse the duplicate with 409, but by then the mapping already points at the request that is about to be closed:

1. Request 0 registers `namespace -> 0`.
2. The duplicate registers `namespace -> 2`, overwriting.
3. The subscriber answers 409 and closes request 2. Cancelling it removes only its `#streams` entry, not the namespace mapping it clobbered.
4. Request 0's `PUBLISH_NAMESPACE_DONE` resolves the namespace to request 2. `#closeStream` finds no such stream and returns.
5. Request 0 stays open and its announcement is never withdrawn.

`readNamespaceRequestId` also throws `unknown namespace` for a withdrawal it cannot resolve, which is fatal to the session.

Pre-existing on `main`: the 409 has always been after the decode, so restoring or removing it changes nothing here. Found while reviewing #2803, which keeps the duplicate refusal for exactly draft-14/15 for a related reason (a second request there is never a legitimate second source, since inline `NAMESPACE` does not exist before draft-16).

The fix belongs in the adapter: don't overwrite a live mapping (first wins while its request is open), or key the reverse lookup so concurrent requests for one namespace stay distinguishable. A regression test needs to go through `ControlStreamAdapter` rather than `NativeSession`, which bypasses it.

Only reachable on draft-14/15 against a peer that sends a duplicate `PUBLISH_NAMESPACE`, so it is not urgent; we negotiate draft-19 by default.

## Closes

- [#2806](https://github.com/moq-dev/moq/issues/2806) - close this issue when the quest finishes
