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

Decided 2026-10-31 (maintainer): investigate the failing Shipping sizes guard
before the release cut. The pre-cut investigation below is complete; this
file now tracks the recurring post-release refresh. Its release-cut blocker
applies to that refresh, not to the incident investigation.

After the release ships, compare its first complete Shipping sizes report
with these baselines. Re-measure the local native profiling rows from
[the previous audit](https://github.com/moq-dev/moq/pull/6541) using the same
Rust target, profile, stripping, and toolchain. Never compare an archive to
an ELF, different targets, or symbol-preserving FFI to fully stripped code.
Preserve artifact keys and history identity; no guard limit or history reset
is needed for this investigation.

### Shipping comparison

The [2026-10-31 Nightly](https://github.com/moq-dev/moq/actions/runs/41204380716)
measured source `38566f5a84cb07bb756aae43f6dc902f1abacec2`. Its last retained
comparison was 2026-10-24, source
`0513c07369357ae0ecd6109d0f6afc61d16b597f`. These are a week's changes,
not one night's. Counts below are the actual uploaded consumer artifacts,
except the explicitly labeled minimal native libraries.

| Shipping artifact, bytes | October 24 | October 31 |
|---|---:|---:|
| JS net, first-load gzip | 120,578 | 131,739 |
| JS hang, first-load gzip | 145,814 | 157,500 |
| JS watch, first-load gzip | 161,132 | 173,880 |
| JS watch-ui, first-load gzip | 173,539 | 186,338 |
| JS publish, first-load gzip | 158,683 | 169,794 |
| moq-c macOS aarch64 tar.gz | 12,117,666 | 12,802,828 |
| moq-c Linux aarch64 tar.gz | 14,139,421 | 14,864,395 |
| moq-c Linux x86_64 tar.gz | 14,484,602 | 15,252,992 |
| moq-c Windows x86_64 zip | 17,211,584 | 18,080,501 |
| Python sdist | 7,733,152 | 9,192,166 |
| Swift generated source tar.gz | 84,457 | 89,070 |
| Minimal FFI aarch64 Linux .so | 23,783,232 | 24,757,488 |
| Minimal FFI aarch64 Linux .a | 56,329,042 | 58,158,580 |

The new generated-C archive keys start their own series. They do not replace
the existing C archive measurements. Native minimal builds remain below the
5% guard. JS raw and Brotli rows grow alongside gzip; the watch CMAF lazy
chunk stays essentially unchanged at 2,459 bytes gzip.

### Python source packaging

[moq-dev/moq#6867](https://github.com/moq-dev/moq/pull/6867) included the Rust
workspace because Cargo resolves disabled optional path dependencies that
Maturin's enabled dependency graph omits. Keep that correctness fix. It also
made media test fixtures in other crates reach the source archive.

On one local host with Maturin 1.14.1, repacking the same source before and
after excluding all `test_data` directories reduced the archive from
9,188,183 to 7,771,007 bytes. The removed mux/video fixture payload is
3,665,958 uncompressed bytes. Audio fixtures were already excluded. The
repacked archive is within 5% of the retained 7,733,152-byte baseline.
Local repack counts differ slightly from hosted archive bytes; use the
matched local pair to quantify the reduction.

The reduced archive retains every Rust workspace source and optional path
dependency. A release wheel built from its extracted contents, installed in
a fresh virtual environment, and imported successfully. Cargo metadata with
all features also resolves from the extracted archive. Existing Nightly
source-install coverage exercises the same packaging boundary.

### Browser first-load attribution and rejected reduction

`bun sh/size/bundles.ts OUTPUT` against the built packages reproduced every
October 31 byte count exactly. Attribute only the entry's static import
closure, not all metafile outputs: IETF session chunks remain lazy.

The largest added first-load input is `sorted-btree`, 19,371 minified raw
bytes, from [moq-dev/moq#6869](https://github.com/moq-dev/moq/pull/6869).
Other net additions include auth session handling (3,902), resume (2,183),
lite subscribe (1,557), track (1,470), reconnect policy (1,446), and group
handling (1,437). These contributions are raw module attribution, not
additive compressed deltas. Hang adds health reporting (4,080 raw bytes);
watch adds player behavior (3,903). Hashed generated filenames must be
normalized before comparing the watch/publish metafiles.

A scratch adaptation to `@js-sdsl/ordered-map` 4.4.2 passed all ten ledger
tests. Its isolated browser module was 10,757 raw / 3,445 gzip bytes, versus
19,886 / 5,952 for the current tree under the same export harness. That is
only a candidate dependency comparison, not a measured consumer-bundle delta.

Five alternating warmed Chromium 156.0.8078.4 rounds of the existing ledger
benchmark measured the following medians at 32,768 sparse arrivals:

| Order, ns per arrival | Current | A/A | Candidate |
|---|---:|---:|---:|
| Ascending | 406 | 403 | 592 |
| Descending | 565 | 562 | 620 |
| Shuffled | 1,080 | 946 | 1,022 |
| Rolling expiry | 700 | 677 | 882 |

Keep `sorted-btree`: the candidate slows ascending and rolling paths beyond
the A/A spread. Do not restore array-backed insertion or full gap scans to
save bytes; the ordered indexes bound sparse ledger work. No dependency or
runtime behavior changed in this investigation.

### Native and generated binding attribution

The actual x86_64 Linux C archive contains a header, one static library, and
its CMake/pkg-config metadata. The library grew from 58,705,866 to 61,628,112
bytes; the header from 147,069 to 163,216. There is no duplicate runtime or
extra build artifact in the package.

`nm -S -C --defined-only` on the two shipped static libraries attributes the
largest text-symbol increases to `moq_net` (+335,565 bytes), `moq_mux`
(+208,475), and `mp4_atom` (+200,070). This is approximate namespace
attribution, not a compressed-byte decomposition. Cross-crate inlining,
compiler-generated code, and metadata account for other bytes. The existing
[catalog publish path quest](/quest/m2/catalog-publish-once.md) owns the
serialization duplication lead. The [bindings size profile
quest](/quest/m2/ffi-size-profile.md) requires codec/crypto and FFI throughput
measurements before adopting opt-level s; no compiler profile was changed.

The Swift source tarball grows in `Generated.swift`, from 684,643 to 704,105
bytes. Its new request/publication, token, media-read and decoder-start
bindings match the changed public API. `Package.swift` only changes the
xcframework checksum, at the same length; licenses and README are identical.
No packaging duplication was found.

### Local WASM baseline

With pinned Rust 1.98.1, `just js wasm` (`wasm-release`, wasm-bindgen web)
produces 2,771,421 raw bytes and 940,444 bytes through `gzip -n -9`, versus
2,611,008 / 885,186 at the previous local source
`78e5b329458e5823415e0fe3f43678170b1df57b` on October 23. The scheduled
history still has no WASM row. Compare future local builds by this method;
adding a scheduled row needs an explicit artifact-set transition.

## Required

- [Release cut](/quest/m2/release/cut.md)

## Related

- [The catalog publish path compiles once](/quest/m2/catalog-publish-once.md)
- [FFI size profile](/quest/m2/ffi-size-profile.md)
- [Native synchronous codecs](/quest/m2/native-compression-sync.md)
- [Native browser codecs](/quest/m2/native-compression-adopt.md)
