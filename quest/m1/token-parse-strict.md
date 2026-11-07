# [S] Rust and JS accept exactly the same tokens and key sets

## Goal

Token and key parsing is strict and identical in `rs/moq-auth` and `js/auth`, and every grant the relay holds has been validated:

- `exp`, `iat` and `nbf` refuse `null` and values outside the range both languages represent. Today Rust reads `"exp": null` as no expiry (a token that never expires) while JS refuses it; the Grant wire `expires` has the same gap.
- A KeySet with a duplicate `kid` is refused at parse in both languages instead of the first match winning.
- A JWK's scope root passes `path::validate` on import in both languages, as the `with_scope` doc promises.
- The relay detects a token the way the server parses one (`presents_token` vs form-decoded `jwt`), so `?%6Awt=<token>` cannot fall through to the public grant; key sources are cached instead of re-read and re-parsed per token.
- `Lease::new` (`rs/moq-relay/src/auth.rs`) runs `Grant::validate`, so an embedder's grant cannot skip the tier charset and upstream-implies-peer checks.

## Plan

Found by the 2026-11-07 invariant audit (auth-2, auth-4, auth-9, auth-11, auth-12). Each refusal gets a vector in the Rust/JS interop test (`js/auth/src/interop.test.ts`), including a null-`exp` token.
