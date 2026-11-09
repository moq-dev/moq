# [XS] JS Key.sign takes exactly Claims

## Goal

`@moq/auth`'s `Key.sign` takes exactly `Claims`, as Rust's
`moq_auth::Key::sign(&Claims)` does: its input no longer accepts the legacy
`put`/`get` role spelling, so each role has one spelling. Tokens it writes
are unchanged: when the legacy form says the same thing, it is still written
that way so older verifiers accept it, and `verify` still reads legacy
tokens.

## Plan

Planned 2026-11-09 (maintainer, /quest-complete follow-up of
[moq-dev/moq#8296](https://github.com/moq-dev/moq/pull/8296), which settles
`Key.sign`'s caps spelling). ✅ Its own small breaking PR in this epic
(rejected: folding it into #8296 mid-iterate; dropping it).

`sign` in `js/auth/src/key.ts` takes `z.input<typeof ClaimsSchema>` plus
the rate fields, so its type still admits whatever the schema parses. Narrow
the parameter to `Claims` (as `verify` returns it), keep the scope check on
the parsed value, and update `js/auth/api/*.api.md`.

Public API: `@moq/auth` `Key.sign` no longer accepts `put`/`get` input.
Migrate: pass `publish`/`subscribe` roles. Wire: none.
