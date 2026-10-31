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

- [moq-dev/moq#7301](https://github.com/moq-dev/moq/pull/7301) adds `moq_auth::mesh::EndpointId` (an Ed25519 key with z-base-32
  text, via `data-encoding` with a custom `Specification`). [moq-dev/moq#7092](https://github.com/moq-dev/moq/pull/7092) adds
  `rs/moq-sign` with `SecretKey`/`PublicKey` using the same specification,
  and a separate JS codec in `js/sign/src/zbase32.ts`. Both chose z-base-32
  so they would share one format; whichever landed second was to dedupe.
- Merge `EndpointId` into `moq_sign::PublicKey` (moq-auth depends on
  moq-sign for it, or the type moves to a small shared crate if the
  dependency direction is wrong; say which in the PR). Keep the shared
  vectors in `drafts/moq-sign-00.json` as the single source of test vectors.
- Note, not in scope: persist a browser JS `SecretKey` (a nonextractable
  `CryptoKey`) in IndexedDB only once a browser mesh node needs a stable
  endpoint ID.

Public API: breaking for moq-auth (`mesh::EndpointId` becomes
`moq_sign::PublicKey` or a re-export). Wire: none.

## Required

- [Enrollment](/quest/m2/mesh/enroll.md) - [moq-dev/moq#7301](https://github.com/moq-dev/moq/pull/7301) adds `moq_auth::mesh::EndpointId`

## Related

- [Mesh](/quest/m2/mesh/README.md) - the epic both types serve
