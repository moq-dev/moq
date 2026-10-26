# [S] moq-auth without the rsa crate

## Goal

moq-auth generates and loads RSA keys through aws-lc-rs and the `pkcs1` crate,
so the workspace graph holds no `rsa` crate and `deny.toml` drops its
RUSTSEC-2023-0071 (Marvin) ignore. Every algorithm moq-auth accepts today still
signs and verifies.

## Plan

Decided by the maintainer on 2026-10-26, when the
[graviola switch](/quest/m3/pure-crypto/README.md) was deferred: `rsa` is only
used for key generation (`generate.rs`) and for turning JWK components into a
PEM (`key.rs`), and signing already runs on aws-lc-rs, but the release should
carry no advisory ignore for it. moq-auth is `rsa`'s only user.

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
