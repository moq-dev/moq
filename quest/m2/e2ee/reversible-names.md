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

- Primitive (2026-10-24 in #6623; the maintainer accepts a slow one for names only): AES-128-GCM-SIV (RFC 8452),
  used only for names. Rust takes aws-lc-rs' `AES_128_GCM_SIV`, already a
  dependency. JS adds `@noble/ciphers` (newest stable) for `gcmsiv`, because
  WebCrypto has no SIV mode. Rejected: an SIV built from WebCrypto's
  HMAC-SHA-256 (synthetic IV) and AES-128-CTR. It needs no dependency and
  was about 2x faster, but the profile would own a bespoke construction
  instead of a standard AEAD with published test vectors. Speed is not a
  factor for one operation per name. Chromium 152, ns per op, seal / open:

  | padded bytes | noble `gcmsiv` | WebCrypto HMAC + CTR |
  |---|---|---|
  | 16 | 10300 / 9200 | 6600 / 5400 |
  | 32 | 11300 / 10700 | 5800 / 6600 |
  | 64 | 12400 / 11800 | 6200 / 6200 |
  | 256 | 18900 / 16300 | 6800 / 6800 |

  Bun and Node, which are only proxies for the browser, put noble at 9 to
  25 us and the WebCrypto SIV at 14 to 30 us.
- `Name` is no longer a fixed 22-character `Copy` value. Parsing checks
  only syntax (base64url, decoding to the tag plus a valid padded length);
  only the generation can open it.
- Keys derive from the semantic name, so the consuming side keeps the
  semantic name it subscribed with rather than deriving keys from the track
  name it receives.
- Recovery is crate-private until
  [undeclared names](/quest/m2/e2ee/undeclared-names.md) uses it.
- Regenerate the vectors with `bun drafts/moq-e2ee-00.ts --write`. Cover the
  padding boundaries (15, 16, 31, and 32 bytes), recovery, a forged name,
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
