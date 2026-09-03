# Path patterns

## Goal

Every predicate over a MoQ broadcast path uses one versioned matcher. Tokens,
origin scopes, announce interests, public access rules, and wildcard
advertisements can express `pid/*/chat` and `**/*.hang` without maintaining
competing glob dialects.

Literal paths remain coordinates, not sets. Roots, joins, exact broadcast
names, URL paths, filesystem paths, and object-store keys keep their own
types. The relay's `/announced/*prefix` debug endpoint remains a prefix-only
exception.

## Plan

### Dialect

A v1 pattern is canonical `/`-separated segments:

- a literal;
- `*`, matching one complete segment;
- `lit*lit`, with one `*` matching bytes inside one segment;
- `**`, matching zero or more complete segments, at most once per pattern.

Patterns are exact by default. `foo` matches only `foo`, `foo/**` matches its
subtree including `foo`, `**` matches every path, and the empty pattern matches
only the current root. Reject leading, trailing, or repeated `/`, more than one
star in a segment, `**` mixed with literal bytes, and more than one `**`.
Literal `*` needs no escape: new path construction and publication reject it,
while decoders tolerate it on legacy protocol versions during rollout.

A pattern list is an unordered union reduced by exact containment. The shared
algebra supplies matching, overlap, exact containment, a literal head, and
exact set-valued rebasing. The set-valued result is load-bearing: rebasing
`**/a` at `a` must preserve both the root match and deeper paths ending in
`a`. Pattern precedence uses one total, Vault-style structural specificity
tuple everywhere rules overlap; equal patterns form the same tier.

### Ownership and compatibility

One dependency-free Rust crate and one TypeScript package own the grammar and
algebra; `moq-net`, `moq-token`, and their JS consumers re-export them. Golden
cross-language vectors, exhaustive small cases, and fuzzing prevent semantic
drift at the authorization boundary. Matching stays linear and inherits
`Path::MAX_PARTS` (32), which also bounds residual expansion.

Every persisted or wire policy carries a version. Missing `v` is v0 prefix
semantics forever. V1 uses exact patterns and rejects legacy and v1 grant
fields mixed in one object. Existing state migrates without changing access:
`foo` becomes `foo/**` and an empty prefix becomes `**`. New SDKs, CLIs, and
APIs default to v1 in a breaking major release; legacy minting is explicit.
The moq.pro (downstream) token minting, public-access migration, scoped-key,
and rule-editor work consume these versioned shapes downstream.

The syntax follows Ant-style path patterns without `?`, classes, or braces.
NATS subjects motivate segment wildcards and reserved wildcard bytes; Vault
ACLs motivate structural specificity. Common Access Token and
`draft-ietf-moq-c4m-01` provide exact, prefix, and suffix matches per namespace
field, including exact depth with a trailing `nil`. Document the exact common
subset and keep the richer MoQ forms explicit rather than claiming CAT cannot
represent `pid/*/chat`.

## Quests

- [Matcher](/quest/m2/path-patterns/matcher.md) - one Rust and TypeScript owner
  implements the grammar and exact algebra
- [Origin scopes](/quest/m2/path-patterns/origin.md) - literal origin roots
  carry arbitrary pattern unions without widening authorization
- [Claims](/quest/m2/path-patterns/claims.md) - versioned token and JWK scopes
  preserve v0 and enforce exact v1 grants
- [Token SDKs](/quest/m2/path-patterns/token-sdk.md) - published libraries and
  CLIs default new minting to v1
- [Relay auth](/quest/m2/path-patterns/relay-auth.md) - relay token, public,
  static, and revalidation paths enforce patterns
- [Pattern interest](/quest/m2/path-patterns/interest.md) - moq-lite-06 carries
  a full pattern in ANNOUNCE_REQUEST

## Related

- [Wildcard advertisements](/quest/m2/wildcard/README.md) - routing adopts the
  matcher while retaining its own cost, pool, refusal, and resolution work
- [JWT-free verdict mode](/quest/m2/auth-verdict.md) - verdict grants use the
  same versioned publish and subscribe pattern sets
