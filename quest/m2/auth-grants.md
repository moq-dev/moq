# [M] A relay refuses a grant it does not fully understand

## Goal

A grant is what the relay enforces, so the relay reads it as strictly as it
reads a token's claims: a grant with a field the relay does not know is
refused and the session is not admitted. Every request to the auth server
carries the relay's product and version, so a server can tell what the relay
reads before it answers. The Rust and JS grant types have the shape of the
wire: one role value holds a direction's patterns and its rate cap, so a
typed server cannot write a cap the relay ignores. A cluster link is one
`link` field instead of the `peer` and `upstream` booleans, and the request's
`Event` and `Role` are open enums.

## Plan

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit):

- ✅ The relay refuses a grant with an unknown field, as it refuses unknown
  claims (rejected: refusing only top-level `publish_rate` and
  `subscribe_rate`; keeping the lenient reader). A field added later may
  narrow a grant, and a reader that ignores it widens the session.
- ✅ The request carries the relay's product and version token, in the
  `name/version` form `implementation` already carries for the client
  (rejected: leaving a server to guess what a relay reads). It is the lever
  a server needs once grants are strict: no token means a relay older than
  0.22, which ignores unknown fields.
- ✅ The Rust and JS grant types take the nested wire shape (rejected:
  keeping `publish` beside `publish_rate`). AGENTS.md nests related fields
  under their shared role, and the wire already does.
- ✅ `peer` plus `upstream` become one `link` enum (rejected: two booleans
  with an invalid fourth state).
- ✅ `Event` and `Role` are `#[non_exhaustive]` (rejected: another break at
  the next event).

Facts found by the audit:

- `wire::Claims` is `deny_unknown_fields` (`rs/moq-auth/src/wire.rs:24`).
  `wire::Grant` is not: "A field it does not know is ignored, so an auth
  server may answer more than this reader takes" (`wire.rs:337-338`), and
  `GrantSchema` is a stripping `z.object` (`js/auth/src/contract.ts:180`).
- Two cases already exist. `upstream` narrows a peer
  (`rs/moq-auth/src/grant.rs:60-63`), and a `release` relay ignores it and
  treats the session as a full peer, while AGENTS.md has the newest and
  previous releases run side by side. The decoded JS `Grant` has
  `publish_rate` and `subscribe_rate`, which are not wire fields, so a
  TypeScript server that returns the typed object gets an uncapped session
  with no error; the package documents the trap ("a bare `publish_rate`
  field is ignored by the relay", `contract.ts:176-178`).
- The flat shape repeats in `moq_auth::Claims` (`claims.rs:130,136`),
  `moq_auth::Grant` (`grant.rs:25,31`), `serve::Rule`, and
  `moq_relay::auth::Token` (`rs/moq-relay/src/auth.rs:444-446`), and is why
  `js/auth/src/wire.ts` exports `decodeRoles`, `nestRates`, `encodeRoles`,
  and `encodeGrants`, and `contract.ts` exports `encodeGrant`.
- `moq auth serve` already says `link = peer|upstream`
  (`rs/moq-auth/src/serve.rs:58-66`); the grant says `peer: bool` and
  `upstream: bool`, with "Requires `peer`" as prose (`grant.rs:58-63`) and a
  `z.refine` in JS (`contract.ts:217-218`).
- `Event` and `Role` have no `#[non_exhaustive]`
  (`rs/moq-auth/src/request.rs:158,233`); `Transport` beside them does
  (`:180`). `Event` gained `Attach` in this window as a break.

For the PR:

- Claims and grants share the role encoding (`wire.rs:279`), so the nested
  role type replaces the flat pair in `Claims`, `Grant`, `serve::Rule`, and
  the relay's `auth::Token` together, in Rust and JS, and the five JS
  helpers go. `moq_auth::Role` is taken by the SETUP direction
  (`request.rs:233`), so pick the nested type's name in the PR, the same in
  both languages. The wire does not change for roles.
- Not decided: the capped role's `path` key holds a list of patterns and
  has never been published. The audit rated a rename low confidence, and
  moq.pro's plans already say `publish.rate`. Recommended: keep `path`.
- `link` fails closed on a relay that predates it only once grants are
  strict, so land both here. `peer` is live on `release`: keep reading and
  writing it as a clearly marked temporary alias that the next version
  migration deletes, and refuse a grant where the two disagree.
- A strict grant is a rollout hazard for a server that already sends extra
  fields. Check `moq auth serve`, the relay tests, and moq.pro's answers
  against the strict reader before merging, and say in `doc/bin/relay/auth.md`
  that a server branches on the relay's token before sending a field newer
  than 0.22.
- A grant the relay cannot read refuses the session and is logged as the
  server's fault, not the client's. Recommended: final for that session
  rather than an outage the client retries, since a retry gets the same
  answer.
- Recommended: the token is a body field next to `node`, not only an HTTP
  header, because the JSON body is the contract servers already parse.
  Name it in the PR.
- JS reads an unknown `transport` as `Unknown` (`contract.ts:32-38`). Give
  `event` the same treatment or say why a server must refuse one.
- Stale text to fix while in these files: `doc/bin/relay/auth.md:100` says
  "There is no shared secret" while the same page documents
  `--listen-secret`, and `rs/moq-relay/src/auth.rs:1009` still explains a
  rule by "the LAN mesh credential", which left for Kixel.
- Tests: the shared Rust and JS corpus gains a grant with an unknown field
  (refused), a grant with both `link` and a disagreeing `peer` (refused),
  and a capped role round trip through the typed API in both languages.
  Update `doc/bin/relay/auth.md` and `doc/lib/js/auth.md`.

Public API: breaking in `moq-auth`, `@moq/auth`, and `moq_relay::auth`
(the role type, `link`, two enums made non-exhaustive, five JS helpers
removed). HTTP contract: a grant with an unknown field is refused, the
request gains the relay token, and `link` replaces `peer` and `upstream`
behind a temporary alias. Wire: none.

## Related

- [Tokens are written as documented](/quest/m2/auth-claims.md) - the token side of the same types
- [An attached token arrives as a token](/quest/m2/auth-attach-token.md) - the other change to the request the server reads
