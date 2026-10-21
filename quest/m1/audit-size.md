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

What to check first: whether the nightly `sizes` job (`sh/size/`) has run.
It needs the Linux release builds, skipped since the 2026-10-11 nixpkgs
bump broke their link. If it has, its history replaces the local method below.
Then whether [moq-net compiles once](/quest/m1/net-erase-transport.md) and
[the catalog publish path compiles once](/quest/m2/catalog-publish-once.md)
landed, and re-attribute the native rows if so.

### Baselines (m0 run, 2026-10-19 and 2026-10-20)

Release assets are x86_64 Linux, stripped as shipped. main is a local build
with the host linker, not `cargo zigbuild`, so compare main to main. JS rows
are `sh/size/bundles.ts` first-load gzip; main is the workspace build.

| Artifact | Earlier release | Release (2026-10-17) | main (2026-10-20) |
|---|---|---|---|
| `moq-relay` | 30.69 MiB (0.14.5) | 37.70 MiB (0.17.7) | 31.29 MiB |
| `moq` CLI | 36.38 MiB (0.8.2) | 49.70 MiB (0.14.7) | 48.14 MiB |
| `libmoq_ffi.so` | 16.80 MiB (0.3.16) | 25.22 MiB (0.4.17) | 31.86 MiB |
| `libmoq` tarball | 10.49 MiB (0.5.15) | 14.29 MiB (0.6.17) | not built |
| `moq_bg.wasm` gzip | ~471 KB (06-09) | not published | 771 KB |
| `net` | 65.3 KB (09-09) | 93.0 KB | 100.9 KB |
| `watch` | 120.3 KB | 160.5 KB | 161.3 KB |
| `watch-ui` | 131.8 KB | 172.6 KB | 173.8 KB |
| `publish` | 94.9 KB | 230.6 KB | 151.2 KB |

Native and WASM main numbers are from 2026-10-19 (2b28f5eac); JS from
2026-10-20, after the fix below.

### Results of the m0 run

- Fixed: `@moq/net` loads the IETF session code on demand, since lite is
  preferred. First load fell 126.8 to 100.9 KB (net), 187.3 to 161.3 KB
  (watch), 199.8 to 173.8 KB (watch-ui) and 176.8 to 151.2 KB (publish).
  `js/net/src/ietf/index.test.ts` fails if the entry statically reaches it
  again.
- Planned: [moq-net compiles once](/quest/m1/net-erase-transport.md), most of
  the native growth since July (the drivers are monomorphized per transport and
  per instantiating crate), and
  [the catalog publish path compiles once](/quest/m2/catalog-publish-once.md),
  about 1 MiB of the CLI.
- Explained, no action: `moq-quic` replacing quinn, `kio`, `unsafe-libopus`,
  `moq-video`/`moq-vaapi` in the FFI library and `fast_image_resize` in
  `libmoq`. The CLI embeds the relay (+5.4 MiB since 0.12.0) and defaults to
  WebRTC (about 1.2 MiB) and the `s3`, `gcs` and `azure` stores; main drops
  the `iroh` default from both. The FFI library keeps its symbol table on
  purpose for UniFFI (about 3.5 MiB). WASM growth is `moq-net`'s.
- `@moq/publish` 0.5.x bundles `mediabunny` eagerly; main fixed it in
  [moq-dev/moq#5267](https://github.com/moq-dev/moq/pull/5267), not yet
  released when measured.

Candidates for next run: the eager IETF SETUP code in `@moq/net` is still
29.9 KiB raw, mostly `parameters.ts`, whose SETUP options share a module with
the request parameters (and `filter.ts`); splitting them needs `cluster.ts`,
`token.ts` and `request.ts` reshuffled, for about 4 KB gzip.

Method: `nm` symbol attribution by crate path and by instantiating crate on
unstripped release `.so`/`.a` files and local `profiling` builds, and esbuild
metafiles from `sh/size/bundles.ts` for JS.

## Related

- [FFI size profile](/quest/m2/ffi-size-profile.md) - whether the bindings ship at `opt-level = "s"`
