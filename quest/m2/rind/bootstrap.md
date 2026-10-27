# [M] Bootstrap rind

## Goal

kixelated/rind exists as a GitHub fork of
[ctz/graviola](https://github.com/ctz/graviola), with `graviola` renamed to
`rind` and `rustls-graviola` to `rind-rustls`, moq's repo setup copied on, CI
green, and a `quest/` tree whose quests deliver everything in this Plan. This
quest is done when rind's tree carries that plan; the code lands in rind.

## Plan

Decided with the maintainer on 2026-10-26. Settled decisions move into rind's
quests verbatim, with their reasons, so rind's sessions don't ask them again.

Repo:

- A GitHub fork, keeping graviola's history for a later rebase or upstreaming.
  Fork only: no upstream PRs until rind carries MoQ, then revisit.
- Copy moq's setup, scaled to one Rust workspace: the Nix flake dev shell with
  direnv, the justfile (`check`, `fix`, and a branch-scoped CI selector),
  `check` and release-plz workflows, `deny.toml`, `AGENTS.md`,
  `CONTRIBUTING.md`, `PROMPTING.md`, the quest tree with milestones and its CI,
  and the shared skills. Drop what is moq-specific (JS, bindings, drafts).
- Adapters are separate crates named `rind-<consumer>`, so a third-party major
  bump never forces a rind major bump. The prefix keeps them grouped under
  rind's own name rather than borrowing the consumer's.

Targets, refused rather than falling back:

- Exactly one backend compiles per arch, never both in one binary
  (maintainer, 2026-10-26).
- x86_64 and aarch64 run graviola's backend. x86_64 needs aes, ssse3, avx,
  avx2, adx, bmi2, and pclmulqdq; aarch64 needs aes, sha2, pmull, and neon.
- rind exposes a CPU check that names the missing feature, the shape asked for
  in [ctz/graviola#227](https://github.com/ctz/graviola/issues/227), so MoQ
  fails at startup instead of panicking on its first handshake. Some VPS
  hypervisors mask ADX ([ctz/graviola#222](https://github.com/ctz/graviola/issues/222));
  those hosts are refused, as are aarch64 boards without crypto extensions
  (Raspberry Pi 4, CM4, Zero 2 W), which run the armv7 build instead.
- armv7 (Android and Linux gnueabihf) runs libcrux's verified portable Rust: AES-GCM, ChaCha20-Poly1305,
  SHA-2, HMAC, HKDF, X25519, Ed25519, ECDSA P-256, RSA-PSS, ML-KEM-768. armv7
  is a phone client or a small board publishing a few Mbps, so portable AEAD speed is enough;
  importing aws-lc's armv7 assembly and porting its C was rejected as far more
  unverified code for throughput the role does not need. libcrux is 0.0.x and
  only armv7 depends on it.
- rind fills libcrux's armv7 gaps with verify-only code on public data:
  RSA PKCS#1 v1.5 verify and ECDSA P-384 verify (web PKI chains need both, such
  as Let's Encrypt's R and E intermediates), plus portable SHA-1. Signing with
  those on armv7 is refused.
- Every other target fails to compile.

Primitives rind adds to graviola's backend:

- Raw AES block and ChaCha20 for QUIC header protection and DTLS 1.3 record
  number protection, and AES-CTR for SRTP AES-CM. Start from ctz's `jbp-quic`
  branch ([ctz/graviola#156](https://github.com/ctz/graviola/pull/156)), whose
  ChaCha part is unfinished.
- SHA-1 and HMAC-SHA1, for STUN MESSAGE-INTEGRITY and SRTP
  AES-CM-HMAC-SHA1-80; browsers require them.

Adapters, each covering what MoQ uses today (from the 2026-10-26 inventory):

- `rind-rustls`: TLS 1.3 and 1.2 suites with AES-128/256-GCM and
  ChaCha20-Poly1305, X25519MLKEM768 preferred, X25519, P-256, and P-384 key
  exchange, verification of ECDSA P-256/P-384, Ed25519, and RSA PKCS#1/PSS,
  signing-key loading for all three key types, and rustls's QUIC keys (packet
  keys and header protection). No P-521, as with ring.
- `rind-jsonwebtoken`: jsonwebtoken 11's `CryptoProvider` for HS256/384/512,
  ES256/384, RS256/384/512, PS256/384/512, and EdDSA, signing and verifying.
  The closed [moq-dev/moq#6861](https://github.com/moq-dev/moq/pull/6861) has
  a working graviola provider to start from.
- `rind-rcgen`: rcgen's `SigningKey` with ECDSA P-256 key generation, for
  self-signed and DTLS certificates.
- `rind-object-store`: object_store's `CryptoProvider` (HMAC-SHA256, SHA-256,
  RSA PKCS#1-SHA256 signing for GCS).
- `rind-str0m`: SRTP AES-CM-HMAC-SHA1-80 and AEAD AES-128/256-GCM, SHA-256
  fingerprints, HMAC-SHA1.
- `rind-dimpl`: DTLS 1.2 and 1.3 with AES-GCM and ChaCha20 suites, X25519,
  P-256, and P-384 key exchange, ECDSA P-256/P-384, and record number
  protection.
- `rind-instant-acme`: instant-acme 0.9's `CryptoProvider`
  ([djc/instant-acme#268](https://github.com/djc/instant-acme/pull/268)). It
  waits on that release, a condition quest in rind's tree.

Accepted costs, from [kixelated/rust2#13](https://github.com/kixelated/rust2/pull/13):
ChaCha20-Poly1305 on graviola is 1.7-3x slower per packet than aws-lc (it
matters only for peers without AES hardware), and graviola's RSA key
generation is not constant-time. Re-measure on rind's first release.

Verification belongs in rind's quests: Wycheproof vectors on every backend
including armv7, differential tests against aws-lc-rs as a dev-dependency,
handshake tests through each adapter, and benchmarks against aws-lc-rs per
algorithm.

Public API: none in moq. Wire: none.
