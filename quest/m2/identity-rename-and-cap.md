# [M] Rename Instance to Identity and cap it at 256 UTF-8 bytes on lite-07

## Goal

`Instance` is renamed to `Identity` across the lite-07 draft (every message
that carries it: `ANNOUNCE_START`, `SUBSCRIBE`, `TRACK`, `TRACKS`, `DATAGRAMS`,
and `FETCH` if still present), the hang-04 catalog JSON (`"instance"` to
`"identity"`), `draft-lcurley-moq-hang.md`, `draft-lcurley-moq-e2ee.md`, Rust
(`moq-net`, `hang`, `moq-hls`, `moq-relay`, `moq-cli`, `moq-ffi`, `libmoq`),
JS (`@moq/net`, `@moq/hang`), and language bindings; `moq-lite-07` enforces a
256-byte UTF-8 ceiling on `Identity` everywhere (rejecting non-UTF-8 or >256
bytes as `PROTOCOL_VIOLATION` in all messages, rejecting empty in
`ANNOUNCE_START` and whenever `Minted = 1`, while keeping empty-with-`Minted = 0`
legal in request messages); and `moq-hls` derives a short fixed-length URL
token hashing `Minted` together with the `Identity` text and keys its internal
maps and include lookups on that token.

## Plan

Decided 2026-11-26 (maintainer, `/quest-plan` of the HLS and lite-07 audit):

- ✅ Cap `Identity` at 256 UTF-8 bytes on the `moq-lite-07` wire and hash it in
  `moq-hls` URLs (rejected: 1024 UTF-8 bytes + hash; 256 UTF-8 bytes keeping raw
  `@{instance}` in `moq-hls` URLs; no wire cap). 256 UTF-8 bytes fits common
  public-key encodings (Ed25519, X25519, P-256, secp256k1 in hex, base64url,
  `did:key`, or JWK) while bounding relay claim memory, and hashing in `moq-hls`
  keeps HLS and DASH URLs short and uniform even when a publisher uses a public
  key as its identity.
- ✅ Rename `Instance` to `Identity` across the lite-07, hang-04, and e2ee
  drafts, Rust, JS, and bindings before cutting `moq-lite-07` (rejected:
  keeping `Instance`). Public keys make `Identity` clearer than `Instance`,
  while `Epoch` stays the incarnation counter within one identity.
- ✅ Rename the unreleased hang-04 catalog JSON key `"instance"` to `"identity"`
  alongside the Rust/JS types and drafts before 0.22 freezes hang-04 (rejected:
  keeping `"instance"` in JSON while renaming wire and SDK types).

Scope:

1. **Drafts and wire (`drafts/draft-lcurley-moq-lite.md`, `drafts/draft-lcurley-moq-hang.md`, `drafts/draft-lcurley-moq-e2ee.md`, `rs/moq-net`, `js/net`):**
   - Rename `Instance` to `Identity` in every lite-07 message that carries it
     (`ANNOUNCE_START`, `SUBSCRIBE`, `TRACK`, `TRACKS`, `DATAGRAMS`, and
     `FETCH` if not yet removed), in `draft-lcurley-moq-hang.md` (lines 136,
     215, 608, 626, 697), in `draft-lcurley-moq-e2ee.md` (line 140), plus
     prose, changelog, and wire fixtures (`rs/moq-net/fixtures/lite.json`).
   - Enforce the 256-byte UTF-8 ceiling in `Identity::from_str`,
     `TryFrom<String>`, and both lite-07 codecs (Rust and JS) across all six
     messages (`PROTOCOL_VIOLATION` on non-UTF-8 or >256 bytes). Keep
     empty-with-`Minted = 0` legal in request messages (`SUBSCRIBE`, `TRACK`,
     `TRACKS`, `DATAGRAMS`, `FETCH` if still present) for "whichever wins the
     path"; empty is a `PROTOCOL_VIOLATION` in `ANNOUNCE_START` and whenever
     `Minted = 1`.
   - Update `just test interop --all` and `just drafts check`.
2. **Hang catalog, Rust, JS, and bindings:**
   - Rename the hang-04 catalog JSON key `"instance"` to `"identity"` and
     update Rust (`rs/hang/src/catalog/{include,video,audio,json,binary}.rs`)
     and JS (`js/hang/src/catalog/*.ts`, `InstanceSchema` to `IdentitySchema`),
     enforcing the 256-byte UTF-8 ceiling on catalog parsing as well.
   - Rename `Instance` types, fields, methods, and errors (`InvalidInstance`,
     `Route::instance`, CLI flags/output, `moq-ffi`, `libmoq` (`rs/libmoq`,
     `c/`), and Go/Python/Dart/Swift/Kotlin bindings) to `Identity`.
   - Keep `Minted` and `Epoch` unchanged beside `Identity`.
3. **`moq-hls` URL hashing and `Minted`-aware lookup keys (`rs/moq-hls`):**
   - Today `Broadcaster::start` (`src/export/mod.rs:187`), `Server`'s `by_id`
     map (`src/server/mod.rs:110`), and `export/include.rs` (`Instance.id` at
     lines 222, 253) drop the `Minted` flag via `as_str()`/`.to_string()`, so a
     minted identity and a named identity with the same text collide.
   - Derive the short fixed-length URL-safe token (matching `init.{hash}.mp4`
     and `seg/{tag}.{n}.m4s`) by hashing the `Minted` flag together with the
     `Identity` text while the typed `Identity` is still available, and use
     that token consistently for `Broadcaster`'s identity, `Server`'s `by_id`
     key, `export/include.rs` (`Identity` lookup), and the `@{token}` path
     segment in `master.rs`, `mpd.rs`, `route.rs`, and `server/routes.rs`.
   - Update `MIGRATE.md`, `doc/bin/hls.md`, and integration tests (including a
     test proving a minted identity and a named identity with identical text do
     not collide in `Server::by_id` or include lookups).

Public API: breaking rename `Instance` to `Identity` across Rust, JS, hang-04
catalog JSON, and bindings, plus `moq-hls` URL token format. Wire:
`moq-lite-07-wip` renames `Instance` to `Identity` and enforces a 256-byte
UTF-8 ceiling across `ANNOUNCE_START`, `SUBSCRIBE`, `TRACK`, `TRACKS`,
`DATAGRAMS`, and `FETCH` (if still present).
