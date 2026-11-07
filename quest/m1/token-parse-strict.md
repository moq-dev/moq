# [S] Rust and JS run one token and key-set corpus

## Goal

`rs/moq-auth` and `js/auth` load one shared corpus of tokens, key sets and JWKs, each marked accepted (with the resulting grant) or refused, so a parser difference fails a test on both sides. The cases this audit found pass:

- `exp`, `iat` and `nbf` refuse `null` and values outside the range both languages represent. Rust reads `"exp": null` as no expiry, a token that never expires, while JS refuses it; the Grant wire `expires` has the same gap.
- A key set with a duplicate `kid` is refused in both languages instead of the first match winning.
- A JWK's scope root passes `path::validate` on import in both languages, as the `with_scope` doc promises.

Two relay-side gaps found beside them are fixed in the same PR:

- The relay detects a token the way the server parses one (`presents_token` vs the form-decoded `jwt`), so `?%6Awt=<token>` cannot fall through to the public grant, and key sources are cached rather than re-read per token.
- `Lease::new` (`rs/moq-relay/src/auth.rs`) runs `Grant::validate`.

## Plan

Found by the 2026-11-07 invariant audit (auth-2, auth-4, auth-9, auth-11, auth-12).

Decided 2026-11-07 (maintainer, invariant-audit root causes): the cause is two hand-written parsers checked only by JS loading a few Rust-signed fixtures (`js/auth/src/interop.test.ts`). The corpus replaces those fixtures and runs in both directions.
