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

Completed the m1 run on 2026-10-23 in
[moq-dev/moq#6541](https://github.com/moq-dev/moq/pull/6541), after transport
erasure landed. Start the next run with the post-release Shipping sizes history,
then remeasure the local-only rows below. Do not compare archive bytes to raw
ELFs, different targets, or symbol-preserving FFI builds to fully stripped ones.

### Shipping baseline

The first scheduled Nightly after the Linux release-link fix has a successful
[Shipping sizes job](https://github.com/moq-dev/moq/actions/runs/39779729461/job/119868272494),
from source `33940b6e336133a64d8601f42833f91708a7fd35` on 2026-10-22.
Its `nightly-size-history` artifact contains one report, so it establishes a
baseline, not a growth trend. The overall workflow failed other jobs; the size
job succeeded. Preserve the existing artifact keys and history identity.

Selected shipping rows, in bytes:

| Artifact | Target | Bytes |
|---|---|---:|
| relay release tar.gz | x86_64 Linux | 14,899,418 |
| CLI release tar.gz | x86_64 Linux | 22,741,534 |
| moq-c release tar.gz | x86_64 Linux | 17,734,176 |
| Go FFI staticlib | x86_64 Linux | 76,134,508 |
| Python wheel | x86_64 Linux | 14,918,122 |
| Kotlin Android AAR | multi-ABI | 43,550,022 |
| Kotlin JVM jar | multi-platform | 56,395,891 |
| Swift xcframework zip | Apple | 52,562,215 |
| minimal FFI .so | aarch64 Linux | 29,975,280 |
| minimal FFI .a | aarch64 Linux | 67,817,364 |

These are the uploaded consumer packages, except the explicitly labeled minimal
FFI builds. The report predates transport erasure. Check the first subsequent
successful Shipping sizes report for its actual shipping effect; do not infer
it from a local ELF or the earlier experiment below. Hosted build durations are
recorded but are not deterministic size or runtime-performance comparisons.

### Local native and WASM baseline

Source `78e5b329458e5823415e0fe3f43678170b1df57b`, measured on 2026-10-23
with pinned Rust 1.98.1 / LLVM 22.1.8 and Bun 1.4.2 in Nix. The empty claim
`d2e15d270fb681b81ac7317329a3b13bab5aa5bb` has the same source tree.
Native rows target the x86_64-unknown-linux-gnu host; WASM rows target
wasm32-unknown-unknown. Remeasure on the same targets or start a new baseline.
The relay, CLI, and FFI rows were each built separately with default features
using
`cargo bloat --locked --profile profiling -p PACKAGE --crates -n 0 --message-format json`
(`--bin moq-relay`, `--bin moq`, or `--lib`). The C archive row uses
`cargo build` instead, as its Method column says. The profiling profile
inherits release's fat LTO, one codegen unit, and opt-level 3, retaining line
tables for attribution. The local linker is rust-lld; shipping Linux builds use
cargo-zigbuild.

| Local artifact | Bytes | Method |
|---|---:|---|
| relay ELF | 23,930,392 | profiling, objcopy --strip-all |
| CLI ELF | 45,427,176 | profiling, objcopy --strip-all |
| FFI .so with UniFFI symbols | 28,039,912 | profiling, objcopy --strip-debug |
| FFI .so, diagnostic only | 23,770,704 | profiling, objcopy --strip-all |
| C static archive | 226,828,912 | cargo build --locked --profile profiling -p moq-c; unstripped |
| moq_bg.wasm | 2,611,008 | just js wasm; wasm-release; wasm-bindgen web |
| moq_bg.wasm gzip | 885,186 | gzip -n -9 |

UniFFI requires the regular symbol table. Its 4,269,208-byte difference here
is metadata, not executable code that bindings can safely discard. The C
archive also includes profiling debug data; it is not a shipping archive.

Cargo-bloat's approximate symbol-owner attribution now assigns `moq_net`
3,522,245 bytes of relay text, 3,576,453 bytes of CLI text, and 3,192,725 bytes
of FFI text. Total text is 18,919,097 / 36,121,184 / 18,526,958 bytes. The old
per-transport driver copies are gone. The prior
[erasure experiment](https://github.com/moq-dev/moq/pull/6176) reported
31.84 to 22.48 MiB relay, 49.06 to 42.48 MiB CLI, and 27.95 to 21.78 MiB
fully stripped FFI. Those were earlier revisions, not a matched A/B against
this run; do not report their difference from this table as new growth.

WASM was about 861.9 KB gzip at the previous local `7d2a91fe66` baseline.
Current source adds listing, discovery, resume, range, TRACKS and IETF
compression behavior while erasing duplicated transport drivers. A diagnostic
name-preserving wasm-release build attributes roughly 1.73 MB of function
bodies to names containing `moq_net`, 53 KB to `zlib_rs`, and 44 KB to
`moq_pattern`. This is approximate current attribution, not a matched byte
delta: preserving names changes retained exports during wasm-bindgen processing.
No codec/profile change is justified by these sizes alone.

The scheduled history currently has no WASM row. Keep the local method for
this run's successor; if adding scheduled WASM coverage, review the artifact-set
and history transition explicitly rather than silently resetting the existing
history or treating the missing row as zero.

### Browser bundles and compression

`bun install --frozen-lockfile`, `just js build`, then
`bun sh/size/bundles.ts OUTPUT` measures real first-load consumer graphs using
esbuild browser ESM splitting/minification and the reporter's gzip defaults.
These two columns use the same method, unlike the native shipping/local rows.

| First-load gzip | Scheduled 33940b6e33 | Local 78e5b32945 | Change |
|---|---:|---:|---:|
| net | 114,477 | 120,016 | +5,539 |
| hang | 139,334 | 145,757 | +6,423 |
| watch | 155,204 | 161,392 | +6,188 |
| watch-ui | 167,562 | 173,732 | +6,170 |
| publish | 152,383 | 158,468 | +6,085 |
| watch/cmaf, lazy | 4,417 | 4,417 | 0 |
| watch-ui/cmaf, lazy | 4,417 | 4,417 | 0 |

Metafile input deltas are concentrated in listing (+3,978 raw bytes), the
pattern index (+3,558), resume (+3,148), lite subscriber (+2,025), publisher
(+1,221), and tracks (+996). Compressed totals are not additive. Pako's raw
contribution is unchanged; it is not the source of this interval's increase.
The existing `js/net/src/ietf/index.test.ts` protects the lazy IETF boundary.

Isolated minified browser ESM exports of the current flate Encoder, Decoder,
and both cost 8,370 / 10,991 / 14,024 bytes gzip. The combined graph adds
3,033 bytes over decoder-only, retaining the accepted synchronous encoder cost.
Track writes and the lazy `Frame.payload` getter consume these codecs
synchronously. An async browser codec/import would change those contracts;
this audit makes no such rewrite. The separate native browser codec quests
own adoption when the synchronous and flush contracts can be met.

### Next attribution leads

The existing catalog quest owns generic catalog/diff serialization duplication:
current CLI attribution includes 2,637,701 bytes `moq_mux` and 1,606,084 bytes
`moq_json`. The FFI profile quest owns the open size-versus-throughput choice:
its size reduction is measured, but its throughput cost still needs a
benchmark. This audit does not adopt opt-level s or remove symbols. Recheck these leads
against post-release shipping history before proposing more work. There was no
new independently justified runtime fix in the m1 run.

Check first (added 2026-10-28): when [moq-dev/moq#7132](https://github.com/moq-dev/moq/pull/7132)
let the Nightly report the generated C archives, it unmasked growth since
2026-10-24 that no run has explained: every JS bundle grew 6 to 8% (the largest
single input is `sorted-btree`, +19 KB raw, from
[moq-dev/moq#6869](https://github.com/moq-dev/moq/pull/6869)), and the Python
sdist grew 17% (7,733,152 to 9,016,318 bytes).

## Required

- [Release cut](/quest/m2/release/cut.md)

## Related

- [The catalog publish path compiles once](/quest/m2/catalog-publish-once.md)
- [FFI size profile](/quest/m2/ffi-size-profile.md)
- [Native synchronous codecs](/quest/m2/native-compression-sync.md)
- [Native browser codecs](/quest/m2/native-compression-adopt.md)
