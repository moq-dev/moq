# [S] moq-crypto signs on rind

## Goal

moq-crypto generates, signs, and verifies its Ed25519 keys with rind instead
of aws-lc-rs, so its build compiles no C crypto.

## Plan

Decided 2026-11-03 (maintainer, follow-up planning after the 2026-11-03 /quest-afk and /quest-complete runs), milestone:

- m0
- m1
- ✅ m2, in this epic

Model it on [moq-dev/moq#7570](https://github.com/moq-dev/moq/pull/7570),
which moved moq-auth to rind and implements `mesh::Signer` for
`rind::signature::Ed25519KeyPair`.

- Keygen, sign, and verify move to rind; key, signature, and wire formats
  stay byte-identical, checked against keys and signatures made before the
  switch.
- Map rind's errors by hand, as #7570 did, so its types stay out of
  moq-crypto's API.
- `cargo tree -p moq-crypto -e normal,build` holds no aws-lc or ring.
- Compare `cargo bench -p moq-crypto --bench sign` before and after.

Public API: none expected, unless a ready-made key type is exposed (as
`mesh::Signer` names rind's). Wire: none.

## Required

- [moq-crypto replaces moq-sign and moq-e2ee](/quest/m1/moq-crypto.md) - creates the crate (moq-dev/moq#7702)
