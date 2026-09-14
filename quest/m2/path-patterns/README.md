# Path patterns

## Goal

Every predicate over a MoQ broadcast path uses one matcher. Tokens,
origin scopes, announce interests, public access rules, and wildcard
advertisements can express `pid/*/chat` and `**/transcode.pro` without
maintaining competing glob dialects.

Literal paths remain coordinates, not sets. Roots, joins, exact broadcast
names, URL paths, filesystem paths, and object-store keys keep their own
types. The relay's `/announced/*prefix` debug endpoint remains a prefix-only
exception.

## Plan

### Dialect

A v1 pattern is canonical `/`-separated segments:

- a literal;
- `*`, matching one complete segment;
- `lit*lit`, with one `*` matching bytes inside one segment (`*.hang`, `foo*`,
  `foo.*.hang`);
- `**`, matching zero or more complete segments, at most once per pattern.

Patterns are exact by default. `foo` matches only `foo`, `foo/**` matches its
subtree including `foo`, `**` matches every path, and the empty pattern matches
only the current root. Reject leading, trailing, or repeated `/`, more than one
`*` in a segment, `**` mixed with literal bytes, and more than one `**`. A
second star in a segment stays reserved: matching it is still linear, but
containment stops being two string compares. Literal `*` needs no escape: new
path construction and publication reject it, while decoders tolerate it on
legacy protocol versions during rollout.

Construction moves `**` before adjacent `*` segments: `*/**` becomes
`**/*`. Equivalent wildcard placements therefore share one text and identity.

A pattern list is an unordered union reduced by containment. The shared
algebra supplies matching, overlap, containment, a literal head, and exact
set-valued rebasing. The set-valued result is load-bearing: rebasing `**/a` at
`a` must preserve both the root match and deeper paths ending in `a`. Union
containment is per member: a candidate covered only jointly by several members
(`a/**` against `a`, `a/*`, `a/*/**`) is refused, so the check stays linear and
a grant that means a subtree writes `a/**`. Pattern precedence uses one total
structural specificity everywhere rules overlap, ordered by literal segments,
then no `**`, then `lit*lit` segments, then `*` segments, then literal bytes
pinned inside `lit*lit` segments, then literal head length. That order agrees
with containment (a strict superset always ranks lower); equal patterns form
the same tier.

### Ownership and compatibility

`moq-pattern` and `@moq/pattern` own the grammar and algebra; `moq-net`,
`moq-auth`, `@moq/net`, and `@moq/auth` re-export them. Literal `Path` types
stay in `moq-net` / `@moq/net`. Golden cross-language vectors
(`rs/moq-pattern/tests/pattern.json`), exhaustive small cases, randomized round
trips, and the moq-net fuzz harness's `pattern` target prevent semantic drift at
the authorization boundary. Matching is linear and inherits `Path::MAX_PARTS`
(32), which also bounds residual expansion.

Grants and claims carry no version. The [Auth server](/quest/m1/auth/README.md)
line makes `moq-auth` read patterns only: `foo` means exactly `foo`, a
subtree is `foo/**`, and an unversioned prefix credential fails verification.
Translating the prefix credentials a deployment already issued is that
deployment's job at its own edge for a deprecation window, which is what
moq.pro (downstream) does. A wire message that carried prefixes keeps them on
the protocol versions that defined them; only new versions carry patterns.

The syntax follows Ant-style path patterns without `?`, classes, or braces.
NATS subjects motivate segment wildcards and reserved wildcard bytes; Vault
ACLs motivate structural specificity. Common Access
Token and `draft-ietf-moq-c4m-01` provide exact, prefix, and suffix matches per
namespace field, including exact depth with a trailing `nil`. Document the
exact common subset and keep the richer MoQ forms explicit rather than claiming
CAT cannot represent `pid/*/chat`.

## Quests

- [Origin scopes](/quest/m2/path-patterns/origin.md) - literal origin roots
  carry arbitrary pattern unions without widening authorization
- [Pattern interest](/quest/m2/path-patterns/interest.md) - moq-lite-06 carries
  pattern grants in AUTH and full-pattern interest in ANNOUNCE_REQUEST

## Related

- [Wildcard advertisements](/quest/m2/wildcard/README.md) - routing adopts the
  matcher while retaining its own cost, pool, refusal, and resolution work
- [Auth server](/quest/m1/auth/README.md) - pattern claims and grants at
  the authorization boundary, prefix-shaped until Origin scopes lands
