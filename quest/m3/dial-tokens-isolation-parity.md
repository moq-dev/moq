# [S] Isolate extra dial tokens and align Rust/JS Tokens::set with Dials

## Goal

`Dial::present` keeps every token presented for a dial isolated to that dial instead of leaking extra tokens into the shared `Tokens` set (and draining or successor sessions), and Rust and JS agree on how `Tokens::set` interacts with configured `Dials`.

## Plan

Decided 2026-11-28 (maintainer, `/quest-plan` of the pre-0.22 design-gate audit): ✅ fix in `m3` (rejected: leaving extra dial tokens leaking and Rust/JS diverging).

Caused by [moq-dev/moq#9515](https://github.com/moq-dev/moq/pull/9515) (7 rounds, 5 API/behavioral choices finalized under `afk merge`, where a Sonnet second reviewer scored 9/10 in round 2 without reading the new diff).

1. **Isolate extra tokens (`rs/moq-net/src/reconnect/dials.rs:154-156`, `rs/moq-net/src/auth/tokens.rs:179-180`):**
   - Today only the first token passed to `Dial::present` stays scoped to the dial; extra tokens overwrite the shared `Tokens` set and leak into draining or successor sessions. Scope all presented tokens to that dial's session.
2. **Rust/JS `Tokens::set` vs `Dials` parity (`rs/moq-net/src/auth/tokens.rs`, `js/net/src/connection/pool.ts:290-321`):**
   - In Rust, `Connection::tokens()` / `Tokens::set` silently overwrites a signed dial's tokens, while JS ignores `tokens.set` when dials are configured. Pick one consistent rule across Rust and JS (recommended: `Dials` owns dial-time signing, so `Tokens::set` does not overwrite dial-scoped tokens on either side, or errors loudly if misused).
   - Optionally add a cancellation signal on Rust `Dial` matching JS `AbortSignal` if callers need it.

Tests: Rust and JS unit tests verifying multi-token `Dial::present` never leaks to a draining or subsequent dial, and verifying identical `Tokens::set` + `Dials` semantics in both languages.

Public API: none (behavioral alignment). Wire: none.
