# [M] moq-auth on rind

## Goal

moq-auth signs and verifies every JWT algorithm it accepts today through
`rind-jsonwebtoken`, and generates keys, converts JWKs, and draws random IDs
through rind. Its graph holds no aws-lc-rs, `p256`, or `p384`, and token and
key formats are unchanged.

## Plan

Decided with the maintainer on 2026-10-26; see the
[epic](/quest/m2/rind/README.md).

- Install `rind-jsonwebtoken` only when no jsonwebtoken provider is installed,
  never through jsonwebtoken's `rust_crypto` backend (it signs RS* and PS* with
  the `rsa` crate).
- Key generation (HMAC, RSA-2048, P-256, P-384, Ed25519) and PKCS#8/PKCS#1
  conversion move from aws-lc-rs and `p256`/`p384` to rind, keeping the DER
  encoding through `pkcs1`/`pkcs8` that
  [moq-dev/moq#6898](https://github.com/moq-dev/moq/pull/6898) added. Never hand-roll the encoding.
- reqwest switches to its no-provider feature.
- The released-token compatibility tests keep passing unchanged.

Public API: moq-auth's error conversions from aws-lc-rs types change
(breaking). Wire and key formats: unchanged.

## Required

- [rind runs on a Pi 4's aarch64 CPU](/quest/m2/rind/aarch64.md) - the rind release this switches to
