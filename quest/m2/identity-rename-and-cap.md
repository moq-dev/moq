# [M] Rename Instance to Identity and cap it at 256 UTF-8 bytes on lite-07

## Goal

`Instance` is renamed to `Identity` across the lite-07 draft, Rust (`moq-net`,
`moq-hls`, `moq-relay`, `moq-cli`, `moq-ffi`), JS (`@moq/net`), and language
bindings; `moq-lite-07` enforces a wire bound of 1 to 256 UTF-8 bytes on
`Identity` (empty, non-UTF-8, or longer than 256 bytes is a
`PROTOCOL_VIOLATION`); and `moq-hls` hashes `Identity` into a short fixed-length
URL token instead of embedding raw percent-encoded text in every playlist URL.

## Plan

Decided 2026-11-26 (maintainer, `/quest-plan` of the HLS and lite-07 audit):

- ✅ Cap `Identity` at 256 UTF-8 bytes on the `moq-lite-07` wire and hash it in
  `moq-hls` URLs (rejected: 1024 UTF-8 bytes + hash; 256 UTF-8 bytes keeping raw
  `@{instance}` in `moq-hls` URLs; no wire cap). 256 UTF-8 bytes fits common
  public-key encodings (Ed25519, X25519, P-256, secp256k1 in hex, base64url,
  `did:key`, or JWK) while bounding relay claim memory, and hashing in `moq-hls`
  keeps HLS and DASH URLs short and uniform even when a publisher uses a public
  key as its identity.
- ✅ Rename `Instance` to `Identity` across the draft, Rust, JS, and bindings
  before cutting `moq-lite-07` (rejected: keeping `Instance`). Public keys make
  `Identity` clearer than `Instance`, while `Epoch` stays the incarnation
  counter within one identity.

Scope:

1. **Draft and wire (`drafts/draft-lcurley-moq-lite.md`, `rs/moq-net`, `js/net`):**
   - Rename `Instance` to `Identity` in `ANNOUNCE_START`, prose, changelog, and
     wire fixtures (`rs/moq-net/fixtures/lite.json`).
   - Enforce `1..=256` UTF-8 bytes in `Identity::from_str`, `TryFrom<String>`,
     and both lite-07 codecs (Rust and JS), returning `PROTOCOL_VIOLATION` on
     empty, non-UTF-8, or over-length values.
   - Update `just test interop --all` and `just drafts check`.
2. **Rust, JS, and bindings:**
   - Rename `Instance` types, fields, methods, and errors (`InvalidInstance`,
     `Route::instance`, CLI flags/output, `moq-ffi`, `moq-c`, Go/Python/Dart/Swift/Kotlin
     bindings) to `Identity`.
   - Keep `Minted` and `Epoch` unchanged beside `Identity`.
3. **`moq-hls` URL hashing (`rs/moq-hls`):**
   - Hash `Identity` into a short fixed-length URL-safe token (matching
     `init.{hash}.mp4` and `seg/{tag}.{n}.m4s`) for the `@{token}` path segment
     in `master.rs`, `mpd.rs`, `route.rs`, and `server/routes.rs`.
   - Update `MIGRATE.md`, `doc/bin/hls.md`, and integration tests.

Public API: breaking rename `Instance` to `Identity` across Rust, JS, and
bindings, plus `moq-hls` URL token format. Wire: `moq-lite-07-wip` enforces a
256-byte UTF-8 ceiling on `Identity` (`ANNOUNCE_START`).
