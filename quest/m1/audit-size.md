# [S] Artifact size audit

## Goal

Growth in release binaries, the FFI library, WASM, and JS bundles since the
previous run is explained, fixed, or planned.

## Plan

Recurs once per major release (decided 2026-10-19). The PR that finishes
this run also adds the next one with the same file name at the end of the next
milestone's `Required` (m0 to m1, m1 to m2). The successor keeps the Goal
and this paragraph; the rest of its Plan is rewritten with what this run
learned: baselines, results, and what to check first. A successor placed in m2
also lists the [release cut](/quest/m2/release/cut.md) under `Required`, so it
waits for the release in flight to ship. Only `/quest-delete` ends the
recurrence. Fixes land in the run's PR, one commit each, rather than as follow-ups.
Only a change that needs a maintainer decision (public API, scope) or is
independently landable and too large to review alongside the rest becomes a
follow-up through `/quest-plan`.

Checkpointed 2026-10-21, a day after the m0 run, with nothing to plan. The
run waited for moq-net to compile once, which landed in
[moq-dev/moq#6176](https://github.com/moq-dev/moq/pull/6176), so the audit
before the breaking cut sees its result and the rest of m1.

What to check first: whether the nightly `sizes` job (`sh/size/`) has run.
The Linux release-link fix
[moq-dev/moq#6090](https://github.com/moq-dev/moq/pull/6090) merged on
2026-10-22 UTC; verify the first scheduled Nightly after it before treating
the sizes history as available. Once it runs, its history replaces the local
method below. Then
re-attribute the native rows after the erasure, and measure `@moq/net`
with the track compression codec in `js/net`.

### Baselines

Release assets are x86_64 Linux, stripped as shipped. main is a local
`cargo build --release` (rust-lld, not `cargo zigbuild`), so compare main to
main. WASM is `just js wasm` output, `gzip -9`. JS rows are
`sh/size/bundles.ts` first-load gzip on the workspace build.

| Artifact | Release (2026-10-17) | main 2b28f5eac (10-19) | main 7d2a91fe66 (10-20) | checkpoint |
|---|---|---|---|---|
| `moq-relay` | 37.70 MiB (0.17.7) | 31.31 MiB | 31.81 MiB | same |
| `moq` CLI | 49.70 MiB (0.14.7) | 47.54 MiB | 48.45 MiB | same |
| `libmoq_ffi.so` | 25.22 MiB (0.4.17) | 34.16 MiB | 34.80 MiB | same |
| `libmoq` tarball | 14.29 MiB (0.6.17) | not built | not built | not built |
| `moq_bg.wasm` gzip | not published | 771.0 KB | 861.9 KB | same |
| `net` | 93.0 KB | 100.9 KB | 101.2 KB | 95.7 KB |
| `watch` | 160.5 KB | 161.3 KB | 161.8 KB | 156.7 KB |
| `watch-ui` | 172.6 KB | 173.8 KB | 174.3 KB | 169.2 KB |
| `publish` | 230.6 KB | 151.2 KB | 151.7 KB | 146.5 KB |

Earlier releases: relay 30.69 MiB (0.14.5), CLI 36.38 MiB (0.8.2), FFI
16.80 MiB (0.3.16), `libmoq` 10.49 MiB (0.5.15), WASM ~471 KB (06-09),
`net` 65.3 KB, `watch` 120.3 KB, `watch-ui` 131.8 KB and `publish` 94.9 KB
(09-09). The 2b28f5eac column was rebuilt here from the same commit; the m0
run's FFI (31.86 MiB) and CLI (48.14 MiB) figures for it do not reproduce.

### Results so far

- Fixed (m0): `@moq/net` loads the IETF session code on demand.
  `js/net/src/ietf/index.test.ts` fails if the entry statically reaches it.
- Fixed (checkpoint): SETUP options live in `ietf/setup_options.ts`, apart
  from the message parameter codec (`parameters.ts`, `filter.ts`), so a lite
  session never loads them. First load fell 5.1 to 5.5 KB gzip in every
  bundle; the same test fails if SETUP reaches `parameters.ts` again.
- Fixed: moq-net boxes the transport, so its session drivers compile once
  instead of per transport and per instantiating crate
  ([moq-dev/moq#6176](https://github.com/moq-dev/moq/pull/6176)). Stripped
  profiling builds fell 31.8 to 22.5 MiB (relay), 49.1 to 42.5 MiB (CLI),
  28.0 to 21.8 MiB (`libmoq_ffi.so`), 254.6 to 207.8 MiB (unstripped
  `libmoq.a`) and 862 to 786 KB gzipped (`@moq/wasm`). Before it, the relay
  binary crate, `moq-tokio` and the library each instantiated their own copy;
  `-Wl,--icf=all` saved only 0.23 MiB, since the copies differ after LTO.
- Planned: [the catalog publish path compiles once](/quest/m2/catalog-publish-once.md)
  (`moq-json`'s diff serializer, +0.14 MiB of the CLI this run).
- Explained, no action: native growth since 2b28f5eac is `moq-net` features
  (ranges and time anchors, TRACKS, followed instances, compressed tracks):
  +0.41 to 0.53 MiB of text per binary. The CLI adds import stats (+0.08 MiB)
  and `moq-mux` (+0.06 MiB); `usage-rs` 7 saves 0.07 MiB. WASM code grew
  187 KB raw: `zlib-rs` 55 KB (compressed tracks; `miniz_oxide` is refused for
  its sync flush bug), `moq-net` 88 KB, `hashbrown`/`hashlink`/`foldhash`
  23 KB. JS grew 0.3 to 0.5 KB gzip.
- Explained in m0, unchanged: `moq-quic` replacing quinn, `kio`,
  `unsafe-libopus`, `moq-video`/`moq-vaapi` in the FFI library,
  `fast_image_resize` in `libmoq`, the relay and WebRTC embedded in the CLI,
  the object-store defaults, and the FFI symbol table kept for UniFFI.

Leads:

- `pako` costs `watch` and `publish` 12.8 KB gzip through `@moq/json`. A player
  only inflates: `Inflate` alone is 9.9 KB, and the browser's
  `DecompressionStream("deflate-raw")` would be free if it yields each
  sync-flushed frame. Synchronous track writes retain the static encoder; the maintainer accepted its measured 3 KB gzip cost.
- `zod/mini` is 9 KB raw of `@moq/net`'s first load for one branded bigint
  (`HopSchema`), and `@moq/hang` imports it as both `zod/mini` and
  `@zod/mini`. Not worth a quest alone.

Method: `nm` symbol attribution by crate path and by instantiating crate (the
v0 mangling suffix) on local `profiling` builds of both revisions, the wasm
name section from a `wasm-release` build with `CARGO_PROFILE_WASM_RELEASE_STRIP=none`,
and esbuild metafiles from `sh/size/bundles.ts` for JS.

## Related

- [FFI size profile](/quest/m2/ffi-size-profile.md) - whether the bindings ship at `opt-level = "s"`
