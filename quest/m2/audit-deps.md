# [M] Dependency and alternatives audit

## Goal

Every dependency, vendored ones included, earns its weight: the fewest that
do the job, each current, deduplicated, and the best maintained option. A
dependency whose use is trivial, that another already covers, or that serves a
feature not worth keeping is removed. Only hand-rolled nontrivial non-core code
that a maintained library covers is planned for replacement.

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

Default to removing (decided 2026-11-01, maintainer): whatever doesn't earn its
cost is deleted inline, and the PR body lists each deletion with its measured
cost and why it wasn't worth it. Only what earns its keep is refactored to be
cheaper instead. Prefer fewer dependencies, and count each one's
compile time and transitive tree in its cost. Removing a public API or a user-facing feature stays a
follow-up.

Research the state of the art for each candidate: compare maintenance,
performance, and API fit before planning a swap. Respect Dependabot's 7-day
cooldown for manual bumps too; security fixes bypass it.

### Check first

Dependabot misses three kinds of update, so list them by hand:

- Majors and 0.x minors. `cargo upgrade --dry-run --incompatible allow
  --pinned allow --ignore-rust-version` lists them for every manifest;
  without `--ignore-rust-version` it hides any release above a crate's MSRV.
  `bun outdated -r` covers the workspace; run `bun outdated` in each
  standalone Bun project too.
- `0.0.x` ranges and `npm:` aliases. `npm:@types/web@^0.0.350` sat for four
  months with no proposal.
- Lockfiles Dependabot doesn't know about. Compare `git ls-files '*.lock'
  '*flake.lock'` against `.github/dependabot.yml`. The m1 run found infra/mbx,
  infra/nix, and test/smoke/flake.lock missing, all added that week.
  Bun's coverage is now a `/infra/*` glob, and `just js audit` and
  `just js workers` walk every tracked `bun.lock`.

Deliberately uncovered: `dart/moq` (see the comment in
`.github/dependabot.yml`) and `nix/uniffi-dart-Cargo.lock` (pinned with the
generator).
The uniffi `>=0.33` ignore blocks nothing until 0.33 ships; keep it, since it
ties uniffi to the Go generator.

The QUIC hard fork has no advisory feed: `cargo deny` can't match quinn
advisories against `rs/moq-quic` (quinn-proto) or `rs/moq-sock` (quinn-udp),
as `SECURITY.md` notes. List quinn-proto and quinn-udp advisories (RustSec,
and GHSA on quinn-rs/quinn) and the upstream commits since the last reviewed
one, port any security fix, and record the newest reviewed commit in
`rs/moq-quic/README.md` beside the fork point (`7616e6b2`). Added 2026-10-28
(maintainer) from the second external security audit, instead of a separate
CI watcher. Check GHSA, not only RustSec: three 2026-09-30 quinn advisories
(GHSA-53rm-773f-4q8c, GHSA-wppq-2f6r-wfvm, GHSA-6pp4-4cxf-xf88) were in
GitHub's feed but not the local RustSec database. That audit found every
known fix already in the fork, each with a regression test. Kept as a
manual step on 2026-10-28 after that evidence; a machine-readable ledger
with a failing scheduled job was judged not worth it while the fork is
current.

Open after the 2026-11-01 removal run:

- The lock still carries vt100 0.17 (moq-cli dev-dependency). Re-check its
  cost before upgrading; dependency upgrades alone were not this off-cycle
  run's goal.
- `test/smoke/flake.lock` lands through the Smoke toolchain update workflow,
  which bumps the npm `playwright` pin with it; confirm the coupled PRs land.
- http-cache-reqwest stays on its 1.0 prerelease (alpha.9) until 1.0 ships.

Advisories waiting on upstream:

- ashpd 0.13 pulls zbus 5 and the unmaintained zvariant, zvariant_derive, and
  zbus_names (RUSTSEC-2026-0370, 0371, 0372). Upstream's
  [zbus 6 port](https://github.com/bilelmoussaoui/ashpd/pull/491) merged on
  2026-10-25; wait for a stable ashpd release that carries it.
- basic-toml (RUSTSEC-2026-0374) comes through askama_derive's default
  `config` feature in uniffi_bindgen 0.32, which reads its `askama.toml`.
  It only reaches the generator-only uniffi fork and never ships.
- The root Bun audit uses version-scoped overrides for chokidar 3 to 4
  (removing braces), postcss-selector-parser 6 to 7.1.7, and Vite 5 to 6.4.5
  (also removing vulnerable esbuild). It also overrides glob to 14.0.3 or
  newer (GHSA-3g3m-2fjr-6pw4) and minimatch to 11.0.3 or newer
  (GHSA-g3hv-75wg-m249). Remove each override when its parent declares a safe
  dependency range: remark-cli/unified-args/unified-engine still requires
  glob 10; TypeDoc and vitepress-plugin-llms still require minimatch 10;
  `@npmcli/map-workspaces` also requires glob 10 and minimatch 9, and
  `@npmcli/package-json` requires glob 10.
  `@tailwindcss/typography` and VitePress own the other parser/bundler ranges.
  All seven tracked Bun lockfiles audit clean after the 2026-11-03 repair.
- proc-macro-error2 (RUSTSEC-2026-0173) is unmaintained, not a reported
  vulnerability. moq-auth, moq-crypto, moq-hls and moq-cli reach it through
  rind's libcrux stack, then libcrux-intrinsics 0.0.9's
  `cfg(hax)` dependency on core-models 0.0.7, which still requires hax-lib
  0.3.7 and its build-time hax-lib-macros. Those are the latest stable
  libcrux-intrinsics/core-models releases as of 2026-11-03. The advisory is
  ignored in deny.toml; remove that ignore when core-models migrates to
  hax-lib 0.4 or newer, whose macros no longer require proc-macro-error2.
- paste (RUSTSEC-2024-0436) remains through tikv-jemalloc-ctl. tikv-jemallocator's unreleased main drops it.

### Baselines (2026-11-01 removal run)

Cargo.lock contains 1,115 registry package versions, 98 registry names with
several versions (117 extra), unchanged by this run. Count from the lock,
including optional/platform dependencies, rather than the default-feature
`cargo metadata` graph (974 registry packages). Exclude workspace and git
packages. Root bun.lock goes from 729 to 726 non-workspace names, with 80
names at several versions (91 extra), counting distinct resolved versions
per name. This is a new snapshot, not a like-for-like attribution of growth
since 2026-10-20.

Most duplicates still come from upstream ecosystem transitions: RustCrypto's
older generation through rsa/jsonwebtoken/STUN/SRT; retina's h264-reader 0.8
and bitstream-io 2; srt-tokio's older rand/socket2/derive_more; rustls-acme's
rcgen 0.13 and web client; winit's older platform stack; and libspa's
system-deps 7 beside GStreamer's 9. moq-vaapi still holds bindgen 0.70,
libloading 0.8 and thiserror 1; mp4-atom also holds thiserror 1. Do not treat
all 117 extras as locally removable without checking those consumers.

Bun's same-major duplicates can persist until a parent re-resolves.
`bun update glob minimatch --lockfile-only --ignore-scripts` with Bun 1.4.2
reapplied the existing overrides on 2026-11-03, removing the vulnerable
nested resolutions without changing unrelated package versions. Prefer a
targeted update for a transitive security bump, then verify with
`bun install --frozen-lockfile` and `just js audit`. Re-check the current
package-manager capabilities before attempting broader deduplication.

Removed inline, with no dependency upgrades:

- `rimraf` from all 19 JS package build scripts: use `rm -rf dist`, supported
  by Bun's cross-platform shell. The installed package was 261,819 bytes;
  a warm 30-run nested-output deletion measured median 24.33 ms through its
  Bun CLI versus 2.41 ms through `bun exec rm -rf` on this host. Its glob
  stack remains needed by other tools or the existing override resolution;
  no transitive-package saving is claimed. Preserve those lock entries:
  regeneration rehoists unrelated packages and drops existing safe pins.
- `@zod/mini` from six runtime and two peer declarations: its JS and type
  entrypoints only re-export `zod/mini`. Use that existing dependency
  directly, keeping every package's `zod` declaration. The wrapper costs
  5,615 installed bytes and no exclusive transitive packages. Validate the
  built declarations as isolated package consumers, not only workspace
  source imports.
- `async-mutex` from `@moq/net`: no source, test, emitted type or script
  imports it. Delete the unused runtime declaration; `tslib` remains shared
  with build tooling. Its installed package was 63,046 bytes; no replacement
  or exclusive-transitive saving.
- `hang`'s `regex` dependency: fixed AV1 fields use the existing
  `codec_number` helper and byte patterns, retaining the grammar. Its
  normal/build closure drops from 183 to 179 packages in the workspace's
  default-feature graph: regex, regex-automata, regex-syntax, aho-corasick.
  They remain in Cargo.lock for other consumers. A fresh uncached Nix
  Rust 1.98.1 dev build with line-table debug info measured 3.12 s summed
  compiler-unit time and 23,447,622 bytes of rlibs for those four crates;
  that is standalone cost, not a measured workspace wall-time saving.
  19,598 original/replacement parser comparisons agreed. The existing
  nightly `moq-mux --bench catalog` target now includes short/full/partial
  AV1 parsing. Paired optimized scratch measurements (five million parses
  per case) were 113/248/45 ns before and 14/53/22 ns after; no timing gate.

`cargo shear` found no unused dependency; its unlinked-file warnings are
compile-fail fixtures and feature-gated source, not permission to delete
coverage. Test deletion and optimization belong to
[the concurrent test audit](https://github.com/moq-dev/moq/pull/7600).

The 2026-11-03 Nightly audit repair re-resolved glob 10.5.0 to 14.0.3 and
both minimatch 9.0.9 paths to the shared 11.0.4 resolution under the existing
root overrides. The root and standalone Bun audits now pass. Rust's audit
also passes with the advisory-specific policy above. The 2026-11-01
removal run's full `just ci` was limited by mbx E2BIG on workspace `cargo doc`
([moq-dev/moq#7594](https://github.com/moq-dev/moq/pull/7594)) and the runner
image's denied user namespace. Neither was bypassed for validation.

### Kept on purpose

Re-judged for behavior and replacement cost, not age or upstream similarity.
The small-crate times below are compiler-unit times from the same fresh
standalone build, not incremental workspace savings.

- `tracing-android`: the Android logcat sink is used by the shipped FFI
  runtime. Replacing it means owning logging/FFI safety; another tracing
  adapter only trades dependencies. Host builds do not compile it.
- `humantime-serde`: duration and optional-duration Serde semantics are used
  across CLI configuration, scenes and impairment presets. `serde` and
  `humantime` are already consumers' dependencies; the adapter itself cost
  0.05 s. A manual adapter would duplicate those behaviors.
- `hex`: encoding, strict decoding and decode-to-slice are used across
  catalog fields, TLS fingerprints, persisted secrets and vectors. No
  transitives; 0.10 s. A second encoding library or hand-written validation
  would add maintenance; its error type is also part of existing APIs.
- `send_wrapper`: enforces thread ownership of io_uring's non-Send session,
  streams and groups when adapting to moq-net's Send transport bounds.
  Removing it means owning unsafe Send/Sync and off-thread drop behavior.
  No transitives; 0.03 s.
- `byteorder`: RTMP's vendored chunk/message/AMF codec uses fallible Read/Write
  helpers, including 24-bit integers and exact short-read behavior, across
  many modules. Replacing the I/O adaptation is larger than replacing an
  in-memory endian conversion. No transitives; 0.10 s.
- `assert_matches`: QUIC tests use guarded patterns and value extraction;
  `matches!` cannot replace those call sites. No transitives; 0.03 s.
- `cpu-time` and `stats_alloc`: benchmarks consume thread CPU and allocation
  counts respectively. Wall time cannot replace CPU time, and a custom
  allocator would recreate stats_alloc's instrumentation. Their own units
  cost 0.05/0.03 s; cpu-time also needs libc/platform bindings.
- `unsafe-libopus`: a nontrivial codec with active moq-audio consumers,
  not a helper. The [Opus backend](/quest/m2/audio-opus-backend.md) comparison
  still owns whether a different backend is better; deleting Opus is a
  user-facing feature decision.
- `moq-gst` keeps its Rust 1.92 floor for GStreamer 0.25, above the
  workspace's 1.91 library floor. Do not normalize that manifest to 1.91.
- The QUIC/UDP, RTMP, V4L2, NVIDIA, VA-API, UniFFI and LibAV forks all have
  current transport, gateway, device, binding or codec consumers. Their
  removal would drop behavior or recreate nontrivial platform code;
  upstream porting stays with [vendor-sync](/quest/m1/vendor-sync/README.md).
  OpenH264 and device integrations remain opt-in where already gated.
- Error/primitive derives remain shared infrastructure: `thiserror` is the
  workspace convention; `derive_more` handles existing enum conversions,
  displays and custom Debug output; `num_enum` handles protocol enum
  mappings. No competing derive crate was added.
- JS keeps Zod validation, pako's interoperable compression, sorted-btree's
  ordered range operations, and UUIDv7 generation. A local replacement
  would own nontrivial parsing, compression, balancing or monotonicity.

### Hand-rolled code

Added: the fixed AV1 codec grammar now lives beside the other fixed codec
parsers. Keep it small and preserve malformed-field refusal; the benchmark
and shared catalog parity corpus exercise the consumer.

moq-mux's avcC and hvcC records go through mp4-atom, its H.264 SPS through
h264-reader, and relay metrics through prometheus-client.
Re-checked and kept small or cold: capped doubling backoff loops (moq-tokio
already has a `Backoff`), per-byte hex formatting, the STUN token bucket,
JS deep equality, WebVTT stamps and `jwt=` lookups. Adding a library would
cost more than these local operations. `js/publish` and `js/watch` UI helper
copies remain a source duplication question, not a reason for another
package. Source/generics/generated-code build cost belongs to the concurrent
build-time audit.

When TypeScript 7.1 is stable, re-check whether the DOM includes
`WebTransportConnectionStats` before deleting the local `TransportStats` in
`js/net/src/connection/stats.ts` (from
[moq-dev/moq#6625](https://github.com/moq-dev/moq/pull/6625)).

## Required

- [Release cut](/quest/m2/release/cut.md) - the breaking release ships before the next run
