# [M] Path patterns

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

Grants and claims carry no version. `moq-auth` reads patterns only: `foo` means exactly `foo`, a
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

### Grants on the wire

AUTH grants on moq-lite-06 carry the shared pattern semantics, without
changing older protocol versions. Interest stays a prefix: [#3770](https://github.com/moq-dev/moq/pull/3770)
keeps patterns off the announce wire, so ANNOUNCE_REQUEST and
SUBSCRIBE_NAMESPACE carry the prefix the caller asked for and a wildcard is
an optional filter on the consume side.

AUTH has not shipped in a release yet: it lives on this line. Land patterns
here, before the line merges, so AUTH_OK carries pattern grants (wildcards and
literals alike) from its first release and never ships a prefix-only encoding.
Today's encoder refuses any grant that is not a subtree
(`rs/moq-net/src/lite/auth.rs`), so a literal grant such as `b1.hang` reaches
the client as no grant at all while the relay's origin enforces it correctly;
this quest removes that gap rather than widening the grant to a covering
prefix. Replace the AUTH grant prefixes with patterns in Rust and JavaScript in
the same change, and update the lite draft and fixtures together. Authorize by
exact containment in the subscriber's v1 grant. Cluster peers adopt nothing as a side
effect of this wire work.

Test Rust and JavaScript interop, leading wildcards, `**` zero-segment
matches, containment refusal, and old-version behavior.

## Related

- [Wildcard advertisements](/quest/m1/wildcard/README.md) - routing adopts the
  matcher while retaining its own cost, pool, refusal, and resolution work
