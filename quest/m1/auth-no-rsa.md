# [S] moq-auth without the rsa crate

## Goal

moq-auth generates and loads RSA keys through aws-lc-rs and the `pkcs1` crate,
so no production crate's graph holds the `rsa` crate (`cargo tree -e normal,build
-i rsa` on moq-relay, moq-cli, and moq-room prints nothing). Every algorithm
moq-auth accepts today still signs and verifies.

## Plan

Decided by the maintainer on 2026-10-26, when the
[graviola switch](/quest/m3/pure-crypto/README.md) was deferred: `rsa` is only
used for key generation (`generate.rs`) and for turning JWK components into a
PEM (`key.rs`), and signing already runs on aws-lc-rs, but the release should
not ship the crate. moq-auth is its only production user.

`deny.toml` keeps its RUSTSEC-2023-0071 ignore, reworded as test-only:
moq-auth's dev-dependencies pin the published `moq-auth` 0.2.1 and `moq-token`
0.7 as compatibility fixtures (`tests/released_token.rs`), and both depend on
`rsa`. The ignore goes once those fixtures move to a release without it.

- Generate with aws-lc-rs's RSA key pair (2048 bits, as today), export it as
  PKCS#8, and read the JWK components back with `pkcs1`/`pkcs8`. Never
  hand-roll bignum math.
- Load a private RSA JWK by encoding all eight components with `pkcs1` and
  handing jsonwebtoken the DER.
- Refuse a private RSA JWK that lacks any of `p`, `q`, `dp`, `dq`, or `qi`
  with a clear error (maintainer, 2026-10-26). RFC 7518 section 6.3.2 already
  requires all of them once `p` is present, and WebCrypto and moq-auth's own
  generator always emit them. Today `rsa` recomputes the CRT values, so this
  is a behavior change to note in the PR. Multi-prime (`oth`) keys stay
  unsupported, refused rather than ignored.

Public API: moq-auth's `From<rsa::Error>` and `From<rsa::pkcs1::Error>`
conversions go away (breaking). Wire and key formats: unchanged.

## Checkpoint

Source implemented in [PR #6898](https://github.com/moq-dev/moq/pull/6898).
The settled backend and validation choices above are unchanged. Matching
JavaScript imports now refuse `oth` rather than dropping it; migration notes
cover that behavior, complete private CRT parameters, and the two removed
Rust error conversions. No other conversion API changed.

Before fixing, the new incomplete-private and `oth` tests both failed.
Afterward, all 229 Rust auth tests and 82 focused JavaScript key/interop tests
passed. The actual CLI generated, saved, signed, and verified all 12 algorithms
with owner-only private files, rejected every missing CRT field, and passed
bidirectional Rust/JavaScript interoperability for all six RSA algorithms.
Relay, CLI, and room normal/build trees have no `rsa`; Cargo reports no matching
package for their inverse queries. Published compatibility dev fixtures retain
it, as planned.

Remaining: the required clean-head `just ci` must pass before ready. Main at
`3f3fc1927` has unrelated CARLA rustfmt failures under `demo/carla/bridge`;
the auth-scoped fix passes, and incidental formatting there was restored.
Full `just fix` also fails in `rs/moq-mux/benches/catalog.rs:226`, which references
removed `hang::catalog::MAX_RENDITIONS`. No open fix for either was found.
Keep these fixes separate and keep this PR draft until the required gate clears.
No maintainer policy decision remains in this quest.
