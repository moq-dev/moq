# [M] moq-net: outbound HOP_PATH construction is never validated

## Goal

Implement and verify the behavior tracked in [#3049](https://github.com/moq-dev/moq/issues/3049)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

`HopPath::validate` runs only on decode, so nothing stops us building and sending an outbound HOP\_PATH that a conforming receiver must reject.

`ietf::cluster::HopPath::validate` rejects an empty list and a repeated non-zero Hop ID, but it is only called from `param_decode`. `Advert::forward` appends our own Hop ID and wraps the result without revalidating, and the encode path does not check either. A `broadcast::Route` whose `hops` already carry a duplicate therefore goes out as-is.

That matters because `draft-lcurley-moq-cluster` requires the receiver to treat it as fatal: "A receiver MUST close the session with a PROTOCOL\_VIOLATION if the entries do not exactly fill `Length`, if the list is empty, or if a non-zero Hop ID appears twice." So a malformed chain we construct does not degrade our own routing, it closes the downstream session.

Routes reach the model from more than one place, and only the IETF decode path is checked:

- The moq-lite ingress builds hop chains itself. `OriginList` caps length but does not reject duplicates, so a lite chain may legally carry one and, once shared through the origin, be forwarded to an IETF cluster peer.
- `broadcast::Route` is public with a public `hops` field, so any in-process producer can attach one.

moq-dev/moq#3042 closed the two reachable cases it introduced, both at ingress: `ietf::Subscriber::route` discards a chain already carrying the identity assigned to the peer, and `lite::Subscriber`'s two responder-append sites do the same. Those are per-path guards. The general gap is that validity is enforced where a chain is parsed rather than where one is emitted, so the next route source has to remember the rule again.

Worth considering: validate in the outbound constructor (`Advert::forward`, or the encode path) and have publisher route selection skip a route that cannot be legally advertised, rather than emitting it. A route that is invalid to advertise may still be fine to serve locally, so dropping the route entirely is probably wrong.

Found by Codex during adversarial review of #3042.

## Closes

- [#3049](https://github.com/moq-dev/moq/issues/3049) - close this issue when the quest finishes
