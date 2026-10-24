# [M] Reversible physical track names

## Goal

The Rust and TypeScript e2ee cores derive physical track names and keys as
the draft's Key Derivation section specifies: a physical name is the
AES-128-GCM-SIV encryption of the padded semantic name under the
generation's name key, and AEAD keys bind the semantic name. A producing
generation recovers the semantic name of any physical name issued under it
and refuses every other name without keeping state. The shared vectors in
`drafts/moq-e2ee-00.json` cover the new derivation, and both cores pass them.

## Plan

The design and its rejected options are in the
[E2EE epic](/quest/m2/e2ee/README.md); its review is
[#6623](https://github.com/moq-dev/moq/pull/6623).

- Rust: aws-lc-rs, already a dependency, ships `AES_128_GCM_SIV`. JS:
  WebCrypto has no GCM-SIV, so take a maintained implementation such as
  `@noble/ciphers`' `gcmsiv`, unless the design review chose otherwise.
- `Name` is no longer a fixed 22-character `Copy` value. Parsing checks
  only syntax (base64url, decoding to the tag plus a valid padded length);
  only the generation can open it.
- Keys derive from the semantic name, so the consuming side keeps the
  semantic name it subscribed with rather than deriving keys from the track
  name it receives.
- Recovery is crate-private until
  [undeclared names](/quest/m2/e2ee/undeclared-names.md) uses it.
- Regenerate the vectors with `bun drafts/moq-e2ee-00.ts --write`. Cover the
  padding boundaries (15, 16, and `2^k - 1` bytes), recovery, a forged name,
  a name issued under another epoch, noncanonical padding, and a plaintext
  that is not UTF-8. Update the draft's Test Vectors coverage sentence.
- The name key is outside the per-key AEAD caps (the draft's Bounds
  section); do not count it.
- Fix comments and docs that still call names one-way or 22 characters,
  such as `rs/moq-e2ee/README.md`, the `identity` error docs, and
  `js/e2ee/src/derive.ts`.

Public API: `Name` loses `Copy` and its fixed length. Wire: physical names
and payload keys change under `moq-e2ee-00`, in the in-progress draft -01.

## Related

- [Undeclared track names](/quest/m2/e2ee/undeclared-names.md) - the consumer of recovery
- [Cross-language encrypted proof](/quest/m2/e2ee/interop.md) - runs on the regenerated vectors
