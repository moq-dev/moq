# [M] Dependency and alternatives audit

## Goal

Dependencies are current, deduplicated, needed, and the best maintained option,
and hand-rolled non-core code that a maintained library covers is planned for
replacement.

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

Open at the end of the m1 run:

- vt100 0.17 and 0.18 (moq-cli dev-dependency) were still inside the
  cooldown (published 2026-10-18 and 19).
- web-transport-iroh 0.8.4 takes kio 0.6, removing the kio 0.5 duplicate.
  It shipped the day of the run, so Dependabot's cargo group should land it;
  confirm.
- `test/smoke/flake.lock` lands through the Smoke toolchain update workflow,
  which bumps the npm `playwright` pin with it; Dependabot auto-merge refuses
  a lock-only smoke PR. Confirm those coupled PRs land.
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
  (also removing vulnerable esbuild). Remove each override when its parent,
  remark-cli/unified-args, `@tailwindcss/typography`, or VitePress, declares
  a safe dependency range. The root and Worker lockfiles audit clean.
- paste (RUSTSEC-2024-0436) remains through tikv-jemalloc-ctl and iroh's
  netlink-packet-core 0.8. tikv-jemallocator's unreleased main drops it.

### Baselines (2026-10-20, m1 run)

Count registry packages only: moq-auth and moq-pattern deliberately depend on
their own published versions for compatibility tests, and the uniffi fork is
a git source.

Cargo.lock: 1,081 packages, 91 registry names with several versions (109
extra). Root bun.lock: 660 non-workspace package names, 72 with several
versions (81 extra), counted per name in its `packages` table; the Worker
locks have none. Most duplicates are ecosystem transitions owned upstream:

- RustCrypto's previous generation (digest 0.10, sha2 0.10, hmac 0.12, der
  0.7, pkcs8 0.10): rsa, stun-types, http-auth, srt-protocol, jsonwebtoken.
- retina (moq-rtsp) holds h264-reader 0.8 and its bitstream-io 2,
  rfc6381-codec 0.2, mp4ra-rust, and mpeg4-audio-const chain, plus socket2 0.5
  and rand 0.8.
- srt-tokio / srt-protocol 0.4.4, unpushed since 2024-05, hold derive_more
  0.99, hmac 0.12, rand 0.8, and socket2 0.5.
  [SRT on the shared socket](/quest/m2/one-port/srt-demux.md) drops srt-tokio.
- rustls-acme holds rcgen 0.13, pem 3, and async-web-client (thiserror 1,
  webpki-roots 0.26). winit 0.30 holds rustix 0.38, x11rb 0.13, and objc2 0.5
  until 0.31 leaves beta.
- gstreamer 0.25 moved to system-deps 9; libspa (PipeWire) still holds 7.
- Our own crates: web-transport-iroh 0.8.3 holds kio 0.5 (see above),
  moq-vaapi holds bindgen 0.70, libloading 0.8, and thiserror 1, and mp4-atom
  holds thiserror 1.
- Bun has no dedupe, so same-major duplicates (`@babel/types`,
  `@vue/shared`, `minimatch`, `picomatch`, `yaml`) persist until a parent
  re-resolves. A transitive-only security bump means editing the lock entry
  (version and integrity) and verifying with `bun install --frozen-lockfile`.

### Kept on purpose

- tracing-android: `tracing-logcat` would add rustix 0.38.
- humantime-serde, hex, send_wrapper, byteorder: unreleased for years but
  complete, with no advisory.
- assert_matches, cpu-time, stats_alloc: moq-quic stays close to quinn.
- unsafe-libopus: the [Opus backend](/quest/m2/audio-opus-backend.md) quest
  owns the comparison.
- moq-gst declares `rust-version = "1.92"` for gstreamer 0.25, above the
  1.91 workspace floor, the same way moq-relay and moq-cli carry their own.

### Hand-rolled code

Planned: [H.264 parsing](/quest/m2/h264-reader.md),
[avcC and hvcC](/quest/m2/mp4-atom-config.md), and
[relay metrics](/quest/m2/relay-prometheus-client.md). Reviewed and kept,
each small or cold: about eleven `(d * 2).min(max)` backoff loops (moq-tokio
already has a `Backoff` that moq-relay's cluster loop could reuse), per-byte
hex formatting in moq-auth and moq-hls, the STUN token bucket, the JS
deep-equal pair, and duplicated WebVTT stamps and `jwt=` lookups.
`js/publish` and `js/watch` carry byte-identical UI helpers (`graph.ts` and
near copies), which is duplication rather than a dependency question.
The m1 run came a day after m0's and did not re-review it; check code
added since.
- When TypeScript 7.1 is stable, delete `TransportStats` in
  `js/net/src/connection/stats.ts` and type `getStats()` as the DOM's
  `WebTransportConnectionStats` (from
  [moq-dev/moq#6625](https://github.com/moq-dev/moq/pull/6625)).

## Required

- [Release cut](/quest/m2/release/cut.md) - the breaking release ships before the next run
