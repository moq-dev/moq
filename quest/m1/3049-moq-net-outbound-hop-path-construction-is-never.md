# [M] moq-net: outbound HOP_PATH construction is never validated

## Goal

A hop chain is validated where it is emitted, not only where it is parsed, so
moq-net cannot construct an advertisement that a conforming receiver must
close the session over.

Every outbound path enforces the rule once: the entries exactly fill `Length`,
the list is non-empty, and no non-zero Hop ID appears twice. Publisher route
selection skips a route that cannot be legally advertised instead of emitting
it, and the moq-lite ingress cannot introduce a duplicate that later reaches an
IETF cluster peer.

Boundaries: a route that is invalid to advertise may still be fine to serve
locally, so it is dropped from advertisement rather than from the origin. The
per-path ingress guards added by [#3042](https://github.com/moq-dev/moq/issues/3042)
stay working; this replaces the need for the next route source to remember the
rule again.

## Plan

Use the public issue's scope and implementation notes below as the starting
plan. Reconcile paths and assumptions with the current tree before
implementation.

### Issue context

`HopPath::validate` runs only on decode, so nothing stops us building and
sending an outbound HOP_PATH that a conforming receiver must reject.

`ietf::cluster::HopPath::validate` rejects an empty list and a repeated
non-zero Hop ID, but it is only called from `param_decode`. `Advert::forward`
appends our own Hop ID and wraps the result without revalidating, and the
encode path does not check either. A `broadcast::Route` whose `hops` already
carry a duplicate therefore goes out as-is.

That matters because `draft-lcurley-moq-cluster` requires the receiver to treat
it as fatal: "A receiver MUST close the session with a PROTOCOL_VIOLATION if
the entries do not exactly fill `Length`, if the list is empty, or if a
non-zero Hop ID appears twice." So a malformed chain we construct does not
degrade our own routing, it closes the downstream session.

Routes reach the model from more than one place, and only the IETF decode path
is checked:

- The moq-lite ingress builds hop chains itself. `OriginList` caps length but
  does not reject duplicates, so a lite chain may legally carry one and, once
  shared through the origin, be forwarded to an IETF cluster peer.
- `broadcast::Route` is public with a public `hops` field, so any in-process
  producer can attach one.

[#3042](https://github.com/moq-dev/moq/issues/3042) closed the two reachable
cases it introduced, both at ingress: `ietf::Subscriber::route` discards a chain
already carrying the identity assigned to the peer, and `lite::Subscriber`'s two
responder-append sites do the same. Those are per-path guards. The general gap
is that validity is enforced where a chain is parsed rather than where one is
emitted.

### Design notes

Validate in the outbound constructor (`Advert::forward`, or the encode path) and
have publisher route selection skip a route that cannot be legally advertised.
Making the invalid state unrepresentable is preferable to a check callers must
remember: a constructor that can only produce a valid chain means the next route
source gets the rule for free.

Found by Codex during adversarial review of #3042.

## Closes

- [#3049](https://github.com/moq-dev/moq/issues/3049) - close this issue when the quest finishes

## Related

- [#3060](/quest/m1/3060-moq-net-ban-hop-id-0-from-hop-chains.md) - bans Hop ID 0 from chains outright, and builds on this
