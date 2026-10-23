# [M] Patterns in IETF AUTH_OK

## Goal

Any grant moq-lite's AUTH_OK can tell, moq-transport's can too. A token
granting an exact path (`ch1`) or a one-level wildcard (`live/*`) is admitted
on moq-transport draft 17+ as it is on draft 16 and moq-lite, instead of
being refused with "grant cannot be encoded as one AUTH_OK".

## Plan

✅ Decided 2026-10-23 (maintainer, importing #6566): carry patterns in IETF
AUTH_OK rather than document a subtree-only rule and warn in `moq auth sign`.
Why: `doc/bin/relay/auth.md` already promises exact patterns, and refusing a
URL token only because its informational AUTH_OK can't be encoded makes
estates that issue exact grants fail closed when clients move to draft 17.
This revises the auth epic's "initially prefixes" choice.

✅ Decided 2026-10-23 (maintainer): encode each entry as the same pattern
string lite's AUTH_OK carries, relative to the session root, not a Track
Namespace plus a match kind. Why: one grammar and one codec, so the two
wires can't drift.

- draft-lcurley-moq-auth-01: AUTH_OK's Publish and Subscribe entries become
  patterns, with a changelog bullet. The -00 codec is not kept, but -01 takes
  a new AUTH Setup Option code point so a -00 peer simply doesn't negotiate
  the extension instead of misparsing AUTH_OK; it falls back to the SETUP or
  URL token as any peer without AUTH does.
- `rs/moq-net/src/ietf/auth.rs` encodes and decodes patterns. The refusal for
  a grant that is not a union of subtrees goes away; the refusal for a grant
  too large for one AUTH_OK stays, never trimmed.
- Mirror in `js/net` if it speaks the IETF AUTH extension.
- Test: the #6566 matrix (exact and subtree grants, drafts 16 and 18) admits
  every cell.

Public API: none. Wire: AUTH_OK in draft-lcurley-moq-auth changes shape under a
new AUTH Setup Option code point.

## Closes

- [#6566](https://github.com/moq-dev/moq/issues/6566) - an exact-path grant is refused on draft 18
