# [L] Encryption profile

## Goal

A versioned MoQ E2EE profile defines interoperable payload protection, identity binding, key derivation, and failure behavior.

The normative profile and shared vectors land in this repository so independent TypeScript and Rust implementations can consume the same contract without importing any platform policy.

## Plan

- Compare the IETF Secure Objects draft directly against moq-lite's group/frame model and moq-lite-only datagram path. Reuse its AES-128-GCM and authenticated-object semantics where the identities map exactly; specify and justify the lite/datagram binding rather than silently treating unlike fields as equivalent.
- Specify canonical binary encodings for the application credential, its required 32-byte random root secret, HKDF labels, opaque physical track names, derived key domains, 96-bit nonces, authenticated properties, encrypted payloads, and typed failures. Media frames and datagrams carry only ciphertext plus the 16-byte tag; credential selection is out of band.
- Compress catalog representations before encryption and decrypt before decompression. Define the reduced plaintext ceiling imposed by the authentication tag so every publisher rejects an oversize protected frame or datagram before transport.
- Make generation uniqueness, the strictest exact integer bound across implementations, sequence exhaustion, retransmission, replacement, downgrade rejection, bounded duplicate suppression, late subscription, and application-retained key history normative. Do not add padding, mid-generation rotation, signing, or key distribution.
- Publish language-neutral known-answer and negative vectors for derivation, naming, groups, datagrams, catalog payloads, relocation across every identity dimension, profile downgrade, tag failure, counter exhaustion, and restart with a reused transport sequence.
- Audit the third-party `moq-secure` prototype from [#3023](https://github.com/moq-dev/moq/issues/3023) as prior art. Its independent counter, custom per-frame header, ChaCha20-Poly1305 suite, signing lease, and bytes-only processor are not compatibility requirements.
