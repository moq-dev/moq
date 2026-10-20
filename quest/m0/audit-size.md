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

Starting points (2026-10-19): the nightly `sizes` job (`sh/size/`) fails
on 5% growth against the previous report or 10% against one 30 days old. It
catches step changes, not creep across a release, so compare against the
baselines recorded here. It already attributes FFI growth with
`cargo bloat`. Related: [FFI size profile](/quest/m2/ffi-size-profile.md).

### Checkpoint (2026-10-19, main at 2b28f5eac)

The nightly `sizes` job has never produced a report: it needs the Linux
release builds, which fail to link until
[release builds link](/quest/m0/release-link.md) lands, so it is skipped every
night. These baselines come from published release assets (x86_64 Linux,
stripped as shipped) and local builds of main (host linker instead of
`cargo zigbuild`, so compare main to main, not to the release column).

| Artifact | Earlier release | Release (2026-10-17) | main |
|---|---|---|---|
| `moq-relay` | 30.69 MiB (0.14.5, 07-31) | 37.70 MiB (0.17.7) | 31.29 MiB |
| `moq` CLI | 36.38 MiB (0.8.2, 07-09) | 49.70 MiB (0.14.7) | 48.14 MiB |
| `libmoq_ffi.so` | 16.80 MiB (0.3.16, 09-02) | 25.22 MiB (0.4.17) | 31.86 MiB |
| `libmoq` tarball | 10.49 MiB (0.5.15, 09-14) | 14.29 MiB (0.6.17) | not built |
| `moq_bg.wasm` gzip | ~471 KB (06-09 comment) | not published | 771 KB |
| `net` first load gzip | 65.3 KB (09-09) | 93.0 KB | 124.9 KB |
| `watch` first load gzip | 120.3 KB | 160.5 KB | 185.2 KB |
| `watch-ui` first load gzip | 131.8 KB | 172.6 KB | 197.7 KB |
| `publish` first load gzip | 94.9 KB | 230.6 KB | 174.7 KB |

JS rows bundle the published npm packages the way `sh/size/bundles.ts`
does (`@moq/watch` 0.5.4 and 0.6.3, `@moq/publish` 0.4.7 and 0.5.3), and
main from the workspace build.

Explained:

- `moq-net` is monomorphized per transport and per instantiating crate. Its
  drivers are generic over the session, so each backend (QUIC/WebTransport,
  qmux) gets a full copy, the IETF `ControlStreamAdapter` wrapper doubles the
  IETF half, and every crate that awaits a `moq-tokio` future or calls
  `moq_net::Server`/`Client` with a concrete session codegens its own copy
  (release builds share no generics across crates, and async bodies are
  instantiated where they are polled). On main, `moq-net` (symbols whose path
  is in `moq_net`, wherever instantiated) is 9.1 of 23.5 MiB of Rust text in
  the relay, split across eight copies by `moq_relay` and `moq_tokio`; 6.4 of
  19.2 MiB in the FFI library, half instantiated by `moq_ffi` and half by
  `moq_tokio`; 7.4 of 36.5 MiB in the CLI. Copies beyond the first of an
  identical demangled function add 4.8 MiB of that in the relay, 2.6 MiB in
  the FFI library and 3.0 MiB in the CLI; across all Rust code they add 7.0,
  4.2 and 8.6 MiB.
  This is most of the native growth since July and the FFI 0.4.0 jump.
- The catalog producer, `moq_mux::catalog::Producer<E>`, instantiates the
  `moq-json` diff serializer for `hang` catalog types once per importer
  crate: about 200 KiB each in `moq`, `moq_mux`, `moq_hls`, `moq_srt`,
  `moq_rtc` and `moq_rtmp`, about 1 MiB of the CLI.
- Feature growth: `moq-quic` replacing quinn, `kio`, `unsafe-libopus`,
  `moq-video`/`moq-vaapi` in the FFI library, `fast_image_resize` in
  `libmoq`. The CLI embeds the relay since 0.12.0 (+5.4 MiB) and on main
  defaults to WebRTC (`str0m`, `dimpl`, about 1.2 MiB) and the `s3`, `gcs` and
  `azure` object stores, while dropping `iroh`. main's relay also drops the
  `iroh` default, which is why it is smaller than the release.
- `@moq/net` first load grew from 149 to 345 KiB raw since September, spread
  over new features (origin routing, resume, auth, datagrams). The IETF code
  is 127 KiB of it and is loaded eagerly although lite is preferred.
- `@moq/publish` 0.5.x bundles `mediabunny` (about 470 KiB raw) into its
  first load. Fixed on main by
  [moq-dev/moq#5267](https://github.com/moq-dev/moq/pull/5267), not yet released.
- WASM (`@moq/wasm`, private) is `moq-net` plus one transport; its growth
  is `moq-net`'s.
- The FFI library ships its regular symbol table on purpose (UniFFI and
  downstream generators read it), about 3.5 MiB of the release `.so`.

Remaining, all needing a maintainer decision before they become quests:

- Erase the transport inside `moq-net` so its protocol code compiles once
  (`Driver<S>`, `server::Handshake<S>` and friends are public, so this is an
  API change). Estimated saving: 5 to 6 MiB of the relay, 4 to 5 MiB of
  the FFI library, similar for `libmoq` and the CLI, and some of the WASM.
  Needs the relay fan-out benchmark to price the dynamic dispatch.
- Instantiate the catalog publish path once (in `moq-mux`), without adding a
  `serde_json::Value` round trip on every publish.
- Load the IETF half of `@moq/net` on demand once a session negotiates it.

What to check first next run: whether the nightly `sizes` job ran; if it
did, its history replaces the local method above.
