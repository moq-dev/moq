# [M] JS origin scope drives announce interest

## Goal

A `@moq/net` origin scoped to a prefix asks the peer only for that prefix.
An origin that was never scoped still asks for every namespace. A relay that
ignores an empty `SUBSCRIBE_NAMESPACE` then delivers a namespace the caller
actually named.

## Plan

`Producer.scope(root, patterns)` matches Rust `origin::Producer::scope`: the
root is stripped from paths, and the patterns are what the handle may publish
and subscribe under. `forwardAnnounced` sends one `SUBSCRIBE_NAMESPACE`, and
the moq-lite equivalent, per literal head of those patterns, the same rule as
Rust `interest_prefixes`. Hidden routes stay opted in on those subscriptions.

An unscoped origin still allows `**`. Its head is the empty prefix, so the
session sends one subscription for everything, which is today's behavior.

`doc/concept/moq-lite.md` says the prefixes a session asks for are the
origin's scope. No new page.

## Related

- [IETF announce count](/quest/m1/ietf-announce-count.md) - how an IETF consumer knows that replay is finished
