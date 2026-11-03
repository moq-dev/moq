# [S] One Ed25519 public key type with one z-base-32 codec

## Goal

`moq_auth::mesh::EndpointId` is merged into `moq_sign::PublicKey`: one Rust
type for a mesh endpoint's Ed25519 public key, with one z-base-32 text codec
(52 lowercase characters, zero trailing bits, no other text form) that
moq-auth and moq-sign share. JS has one codec too, used by `@moq/sign` and
by `@moq/auth` if it grows one. The text form, vectors, and refusals stay as
both PRs specified.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31
/quest-complete run), from
[moq-dev/moq#7301](https://github.com/moq-dev/moq/pull/7301) and
[moq-dev/moq#7092](https://github.com/moq-dev/moq/pull/7092).

Milestone: ✅ m1 (moved from m2). Not chosen: m2. There is no m1 mesh epic,
so this sits at the m1 top level; the [mesh epic](/quest/m2/mesh/README.md)
is related.

- [moq-dev/moq#7301](https://github.com/moq-dev/moq/pull/7301) (merged) added `moq_auth::mesh::EndpointId` (an Ed25519 key with z-base-32
  text, via `data-encoding` with a custom `Specification`). [moq-dev/moq#7092](https://github.com/moq-dev/moq/pull/7092) (merged) added
  `rs/moq-sign` with `SecretKey`/`PublicKey` using the same specification,
  and a separate JS codec in `js/sign/src/zbase32.ts`. Both chose z-base-32
  so they would share one format; whichever landed second was to dedupe.
- Merge `EndpointId` into `moq_sign::PublicKey` (moq-auth depends on
  moq-sign for it, or the type moves to a small shared crate if the
  dependency direction is wrong; say which in the PR). Keep the shared
  vectors in `drafts/moq-sign-00.json` as the single source of test vectors.
- The implementation in [moq-dev/moq#7733](https://github.com/moq-dev/moq/pull/7733)
  uses `moq_sign::PublicKey` directly in auth, moves byte access and string serde
  onto it, and removes auth's codec. Auth still verifies proofs on rind. Rust
  auth/signing and JS signing consume `drafts/moq-sign-00.json`.
- [moq-crypto replaces moq-sign](/quest/m1/moq-crypto.md) renames
  `moq-sign`/`@moq/sign` to `moq-crypto`/`@moq/crypto`. If it lands first,
  merge `EndpointId` into its public key type and use its JS codec instead.
- Note, not in scope: persist a browser JS `SecretKey` (a nonextractable
  `CryptoKey`) in IndexedDB only once a browser mesh node needs a stable
  endpoint ID.

Public API: breaking for moq-auth (`mesh::EndpointId` becomes
`moq_sign::PublicKey` or a re-export). Wire: none.

## Waiting

- Maintainer: choose the dependency boundary after local review of
  [moq-dev/moq#7733](https://github.com/moq-dev/moq/pull/7733). The direct
  dependency proposed in that draft makes published `moq-auth` require
  `moq-sign`, whose manifest sets `publish = false`. It also makes auth compile
  net and aws-lc-rs just for the public key, while auth itself verifies on rind.
  Option 1 (recommended): move the existing `PublicKey` and codec to a small
  publishable leaf crate and re-export the same type from signing; settle the
  crate name before implementation. Option 2: make the existing signing crate
  publishable with a key-only dependency path, gating its net and aws-lc-rs
  dependencies. Keep the rename in
  [moq-dev/moq#7702](https://github.com/moq-dev/moq/pull/7702) independent and do
  not settle its timestamp-export decision here. A maintainer comment choosing
  the boundary and naming clears this wait; implement it, rerun checks, and
  review the resulting head before completion.
- Maintainer: settle the extra methods on the removed `EndpointId` before
  completing [moq-dev/moq#7733](https://github.com/moq-dev/moq/pull/7733).
  It exposed `publish()` and raw `verify(message, signature)`, while signing's
  `PublicKey::verify` is private and takes a fixed-size signature. Option 1
  (recommended, implemented in the draft): keep mesh policy and rind proof
  verification in auth, use `Pattern::subtree(&format!("mesh/{endpoint}"))` for
  paths, and use `Proof::verify` or a verifier over `PublicKey::as_bytes()` for
  signatures. This adds no new public helper or verification API. Option 2:
  preserve those conveniences with a public mesh path helper and public raw
  `PublicKey` verification, settling their names and verification contract.
  A maintainer comment on the PR choosing the migration clears this wait.

## Related

- [Mesh](/quest/m2/mesh/README.md) - the epic both types serve
- [moq-crypto replaces moq-sign](/quest/m1/moq-crypto.md) - renames the crate this quest merges into
