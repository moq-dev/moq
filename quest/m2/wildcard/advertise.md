# [L] Advertise

## Goal

moq-net encodes, forwards, and authorizes wildcard advertisements, without yet
resolving one into a subscription.

## Plan

Following the draft, the message goes in `rs/moq-net/src/lite/announce.rs`
alongside `AnnounceBroadcast`, gated on the lite-06 version check the route
cost already uses so older peers neither send nor receive it.

That gate is NOT sufficient on its own. `moq-lite-06-wip` is one ALPN with no
sub-version, and `AnnounceBroadcast::decode` rejects an unknown message type
outright (`DecodeError::InvalidMessage`), which kills the announce stream. A
relay running an earlier Lite06 build therefore negotiates the same version and
then drops the session when a newer peer sends a wildcard. Land the DECODE side
so an unknown announce type is tolerated before any build emits one, and treat
emission as a separate rollout step: no service emits the message until
deployed receivers tolerate it. Test a
sender against the INTERMEDIATE receiver build, the one that recognizes
wildcards but does not emit them: that is what a rollout actually deploys
first. Testing against a receiver built before the message existed only
re-demonstrates that it drops the stream, which is why emission has to wait.

The state belongs in `rs/moq-net/src/model/origin.rs` beside the announce tree
rather than inside it: a wildcard is not a node, it covers paths that mostly
do not exist. Keep it in a flat structure keyed by the pattern, matched by the
shared matcher [Matcher](/quest/m2/path-patterns/matcher.md) provides, and
the set is small (one entry per advertiser per service, not per broadcast).

A wildcard forwards like an announcement: accumulate the sending peer's declared
link cost onto its single varint (`Cost::charged` adds to both halves of a
broadcast's pair; a wildcard has one value because it can never be warm), append
the upstream hop id, discard one whose reconstructed path
contains the receiver's own origin, and apply the same per-subscriber exclusion
so a wildcard is never advertised back through a path that flows through the
subscriber. Retraction and replacement reuse the id-referencing forms.

Bound what a session may advertise by pattern containment against
`Producer`'s granted patterns, which already carry the publish scope the token
granted. An advertisement not contained by them is refused rather than
clamped, so a misconfigured worker fails loudly instead of quietly advertising
less than it thinks. The same exact containment handles literal-headed and
leading-star patterns without a special authorization rule.

Wildcards are visible to subscribers, so `announced` must surface them as a
DISTINCT event rather than as an available broadcast: a pattern names no
content, and presenting one as a broadcast would have consumers subscribe to
the pattern itself. Scope and rebase each one per subscriber with the matcher's
exact set-valued operation. Aggregate duplicate advertisers of one pattern into a
single presented entry whose withdrawal fires only when the last advertiser
leaves. That aggregation has no other owner, so it belongs here.

Tests: encode/decode round trip and the version gate, cost accumulation across
two hops, reflected-wildcard drop, per-subscriber exclusion, retraction, scope
refusal, root and descendant residuals from a `**` rebase, and the distinct
wildcard event itself: never presented as an
available broadcast, duplicates aggregated into one entry, withdrawal firing
only when the last advertiser leaves.

## Required

- [Draft](/quest/m2/wildcard/draft.md)
- [Matcher](/quest/m2/path-patterns/matcher.md) - the shared pattern
  matching, containment, and rebasing this authorizes with
