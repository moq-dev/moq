# [L] Advertise

## Goal

moq-net encodes, forwards, and authorizes wildcard advertisements, without yet
resolving one into a subscription.

## Plan

Following the draft, this goes in `rs/moq-net/src/lite/announce.rs` alongside
`AnnounceBroadcast`, gated on the lite-06 version check the route cost already
uses so older peers neither send nor receive it.

[Draft](/quest/m2/wildcard/draft.md) settles whether a pattern rides the
existing announce messages' prefix field or arrives as a message of its own.
Either way the skew hazard below applies, and either way the decode side lands
first.

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

The state has a home already. Since
[moq#3225](https://github.com/moq-dev/moq/pull/3225) a route is a flat
`RouteEntry` keyed by an opaque `origin::Prefix`, deliberately built as the
extension point a pattern type slots into: matching is segment-wise
intersection in one place, so a pattern extends `Prefix` internally without
touching `Route` or any signature. Teach `Prefix` the dialect the shared
[Matcher](/quest/m2/path-patterns/matcher.md) provides rather than adding a
parallel table beside it. The set stays small either way (one entry per
advertiser per service, not per broadcast).

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

Wildcards are visible to subscribers, and #3225 already made that safe: an
announcement is a covering claim, `Consumer::announced` yields
`announce::Update { prefix, route, active }`, and it never yields a
`broadcast::Consumer`. So a pattern needs no distinct event kind; it is another
covering claim in the stream consumers already read, and no consumer can
mistake one for a broadcast. Scope and rebase each per subscriber with the
matcher's exact set-valued operation. Aggregate duplicate advertisers of one
pattern into a single presented entry whose withdrawal fires only when the last
advertiser leaves. That aggregation has no other owner, so it belongs here.

Tests: encode/decode round trip and the version gate, cost accumulation across
two hops, reflected-wildcard drop, per-subscriber exclusion, retraction, scope
refusal, root and descendant residuals from a `**` rebase, duplicates
aggregated into one entry, and withdrawal firing only when the last advertiser
leaves. Cover that a pattern and a literal prefix coexist in one route table
and that a literal-only deployment behaves exactly as it does today.

## Required

- [Draft](/quest/m2/wildcard/draft.md)
- [Matcher](/quest/m2/path-patterns/matcher.md) - the shared pattern
  matching, containment, and rebasing this authorizes with
