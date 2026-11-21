# [S] Grants stop spelling prefixes as subtree patterns

## Goal

Tokens, key scopes, and auth server grants carry bare prefixes, and `moq-auth` and
`@moq/auth` hold no `foo/**` alias. [moq-dev/moq#9251](https://github.com/moq-dev/moq/pull/9251)
made grants prefixes and added the alias both ways, so 0.22 works beside releases that read
a grant member as a pattern. It comes out in two steps, one per version migration, so no
pair of releases a deployment runs side by side ever narrows or refuses a grant.

## Plan

Decided 2026-11-20 (maintainer, on moq-dev/moq#9251): ✅ a temporary read alias, with the
write alias releases before 0.22 need to read the same subtrees (rejected: shipping strict and
upgrading an auth server with the relays it answers together).

The alias is `alias` in `rs/moq-auth/src/wire.rs`. In JS it is `readPrefix`, `readPrefixes`,
`WirePrefixListSchema`, `WireAccessSchema`, `writtenAccess`, and `writtenScope` in
`js/auth/src/wire.ts`, with their uses: `WrittenAccessSchema` in `js/auth/src/contract.ts`,
`ScopeSchema`'s last transform and `Scope.allows` in `js/auth/src/claims.ts`, and `toPublic` in
`js/auth/src/key.ts`. Each is marked `TEMPORARY MIGRATION`.

1. **Write bare prefixes** in the first version migration after 0.22, so it ships in 0.23.0
   and never in a 0.22 patch (see Waiting). Delete Rust's `Written` and JS's `writtenAccess`
   and `writtenScope`, so `GrantSchema`, `ScopeSchema`, and `Key.public` output bare prefixes,
   and a typed `Grant` or key is again the JSON to write. Keep the read: 0.22 issuers and auth
   servers still write `foo/**`, and `Scope.allows` still reads it. Flip the pins
   (`tests/released_token.rs`, `rate.json`, the grant cross-language vector, the corpus) and
   the wire-compat token lane, where a released (then 0.22) verifier reads both spellings.
   The same PR deletes interop.sh's compat `**` probe, since the released relay then reads
   `public = ""`, and rewrites this quest to step 2.
2. **Delete the read** in the version migration after that: `foo/**` and `**` are refused in
   tokens, key scopes, and grants like every other wildcard, with the message naming the
   prefix to write. Tokens 0.22 minted with an `exp` have expired by then. Delete this quest.

## Waiting

- Maintainer: open the API or wire phase of the cycle after 0.22, before step 1 lands. A
  change landed in that cycle's additive phase would ship in a 0.22 patch, which a relay or
  verifier from before 0.22 reads narrower. Check: `version.json` reads `"minor": "0.22"`
  and `"phase"` is `"api"` or `"wire"`.
- Maintainer: decide what step 2 does with a `/**` spelling nothing re-mints. A key scope
  0.22 wrote, or a token it signed with no `exp`, keeps `foo/**` until someone rewrites it,
  and deleting the read refuses it. Recommended: delete the read everywhere, with a
  migration note to regenerate such keys (or edit `foo/**` to `foo` in the JWK, which is
  plain JSON) and re-mint unbounded tokens before upgrading; the refusal is loud at load.
  Rejected: keeping the read for key scopes alone, which keeps a wildcard spelling alive in
  the one place it is never rewritten. Check: this item is answered in the Plan.
