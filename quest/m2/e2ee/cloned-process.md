# [XS] Cloned publisher processes and key length

## Goal

The E2EE draft and docs say that a cloned process image (a `fork` without
`exec`, a restored VM snapshot or CRIU checkpoint, or one prepared credential
copied to two processes) is a new publisher instance that must mint its own
generation, and `@moq/e2ee` documents that a caller passing a `CryptoKey`
owns the profile's 32-byte secret rule.

## Plan

From the 2026-10-27 security audit (E2EE-A, E2EE-B). A producing generation's
epoch (`rs/moq-e2ee/src/credential.rs`) and per-track counters
(`track.rs`) are copied verbatim by a clone, so two instances encrypt
different plaintext under the same AES-GCM key and nonce. The draft already
makes single-instance ownership a MUST
(`drafts/draft-lcurley-moq-e2ee.md`); nothing can detect a violation cheaply.
The audit's random-epoch idea does nothing, since the epoch is minted once and
copied with the process. Real protection needs per-group random nonce
material, a wire change not worth its cost.

`js/e2ee/src/secret.ts` accepts a non-extractable HKDF `CryptoKey`, whose
length WebCrypto cannot report. A wrong length derives different keys, so
decryption fails loudly rather than leaking.

Decided 2026-10-27 (maintainer): document both. Keep `CryptoKey` input so
secrets can stay non-extractable.

Public API: doc comments only. Wire: draft prose only; run `just drafts
check`.
