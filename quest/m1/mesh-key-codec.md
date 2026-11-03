# [S] One Ed25519 public key type with one z-base-32 codec

## Goal

`moq_auth::mesh::EndpointId` is merged into `moq_crypto::sign::PublicKey`: one Rust
type for a mesh endpoint's Ed25519 public key, with one z-base-32 text codec
(52 lowercase characters, zero trailing bits, no other text form) that
moq-auth and moq-crypto share. JS has one codec too, used by `@moq/crypto` and
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
- Merge `EndpointId` into `moq_crypto::sign::PublicKey` (moq-auth depends on
  moq-crypto for it, or the type moves to a small shared crate if the
  dependency direction is wrong; say which in the PR). Keep the shared
  vectors in `drafts/moq-sign-00.json` as the single source of test vectors.
- [moq-dev/moq#7702](https://github.com/moq-dev/moq/pull/7702) merged
  `moq-sign`/`@moq/sign` into `moq-crypto`/`@moq/crypto`; the JS codec is
  `js/crypto/src/sign/zbase32.ts`.
- Dependency boundary, decided 2026-11-03 (maintainer): ✅ publish moq-crypto
  with a key-only dependency path that excludes moq-net and aws-lc-rs. Not
  chosen: a separate publishable leaf crate. The `sign` and `e2ee` default
  features gate those modules; with default features off, moq-crypto is only
  `sign::PublicKey`/`InvalidKey` and the z-base-32 codec (`serde` optional),
  which moq-auth depends on.
- [moq-dev/moq#7733](https://github.com/moq-dev/moq/pull/7733) implements this:
  auth uses `moq_crypto::sign::PublicKey` directly, the key gains public byte
  access and string serde, and auth's codec is removed. Auth still verifies
  proofs on rind. Rust auth/crypto and JS crypto consume
  `drafts/moq-sign-00.json`.
- Note, not in scope: persist a browser JS `SecretKey` (a nonextractable
  `CryptoKey`) in IndexedDB only once a browser mesh node needs a stable
  endpoint ID.

Public API: breaking for moq-auth (`mesh::EndpointId` becomes
`moq_crypto::sign::PublicKey`). Wire: none.

## Waiting

- Maintainer: settle the extra methods on the removed `EndpointId` before
  completing [moq-dev/moq#7733](https://github.com/moq-dev/moq/pull/7733).
  It exposed `publish()` and raw `verify(message, signature)`, while the
  crypto `PublicKey::verify` is private, needs the `sign` feature (aws-lc-rs),
  and takes a fixed-size signature. Option 1 (recommended, implemented in the
  draft): keep mesh policy and rind proof verification in auth, use
  `Pattern::subtree(&format!("mesh/{endpoint}"))` for paths, and use
  `Proof::verify` or a verifier over `PublicKey::as_bytes()` for signatures.
  This adds no new public helper or verification API. Option 2: preserve
  those conveniences with a public mesh path helper and public raw
  `PublicKey` verification, settling their names, verification contract, and
  backend. A maintainer comment on the PR choosing the migration clears this
  wait.

## Related

- [Mesh](/quest/m2/mesh/README.md) - the epic both types serve
