# [S] @moq/auth key derivation and key sets validate like single keys

## Goal

- `Key.public(key)` returns a key that shares no mutable state with the
  signer, and that `Key.parse` accepts. A key that can't verify is refused
  instead of turned into `key_ops: []`.
- `KeySet.parse` refuses the same keys `Key.parse` does, such as an `oct`
  key declaring `ES256`, so bad configuration fails at setup.
- `rs/moq-auth` refuses them at parse too.

## Plan

Moved to m2 2026-11-12 (maintainer, /quest-audit before the cut): the fix
makes `@moq/auth`'s `Key.public` throw, and `KeySet.parse` and Rust's `Jwk`
parse reject keys they accept today, so it is a break the cut should carry.

Decided 2026-11-08 (maintainer, /quest-plan of an external code audit of `9f51fa0966`): milestone ✅ m3 (rejected: m1), audit scores 5, 3 and 3/10.
Under the cross-package sync rule, Rust's `Jwk::import` also checks
algorithm against key type eagerly. Today that check runs only when a key is
used (`rs/moq-auth/src/key.rs:488-555`), so both languages accept a bad key
at parse and fail later.

Facts at `9f51fa0966`:

- Shared scope: `js/auth/src/key.ts:133`, `:138` and `:143` spread the key,
  so `scope` and its arrays are the signer's own objects, and `sign`
  enforces them (`:220`, `:434`). Clone the scope, as Rust's `to_public`
  does (`key.rs:463`). This needs in-process mutation and isn't a remote
  bypass. A frozen or deep `Readonly` type was considered and rejected: JS
  keys are plain mutable objects everywhere, and tests edit generated keys.
- Sign-only: the filters at `key.ts:134`, `:139` and `:144` turn
  `["sign"]` into `[]`, which breaks `KeySchema`'s `minLength(1)`
  (`:46`). Throw when `verify` is missing, and set `key_ops` to exactly
  `["verify"]`, matching Rust (`key.rs:429-431`, `:460`).
- `KeySet.parse` (`js/auth/src/set.ts:56`) runs only the schema, while
  `Key.parse` also runs `validateKey` (`key.ts:182-186`, `:439-479`).
  Recommended: move `validateKey` into `KeySchema` as a refinement, which
  also merges the duplicated `oct` length check (`:53-64`, `:445-448`).

Public API: `Key.public` throws for a key that can't verify; key sets and
Rust keys that mismatch algorithm and key type fail at parse. Wire: none.
