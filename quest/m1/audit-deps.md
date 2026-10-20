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
cooldown for manual bumps too.

### Check first

Dependabot's grouped weekly PRs only took semver-compatible updates in the m0
run: tikv-jemallocator 0.7 sat unproposed for five months. List majors (0.x
minors included) for every direct dependency with `--all-features` metadata
against the crates.io index, and every `package.json` range against npm.
Also confirm each Bun lockfile is in `.github/dependabot.yml` (infra/moq-sh
was missing) and that every Dependabot `ignore` still blocks something.

Majors still open at the end of m0:

- gstreamer 0.25 needs Rust 1.92; moq-gst is on 0.24 under the 1.91 MSRV.
- vt100 0.17 and 0.18 (moq-cli dev-dependency) were inside the cooldown.
- http-cache-reqwest stays on its 1.0 prerelease until 1.0 ships stable.

Advisories waiting on upstream, with no exceptions or overrides added:

- ashpd 0.13 pulls zbus 5 and the unmaintained zvariant, zvariant_derive, and
  zbus_names (RUSTSEC-2026-0370, 0371, 0372). Upstream's
  [zbus 6 port](https://github.com/bilelmoussaoui/ashpd/pull/491) is open.
- The root Bun audit has six findings in four packages: VitePress 1.6.4 pins
  Vite 5 and its vulnerable esbuild (VitePress 2 is still alpha),
  `@tailwindcss/typography` 0.5.20 pins postcss-selector-parser 6, and braces
  3.0.3 has no fix for
  [GHSA-vfj7-8cjw-p6xm](https://github.com/advisories/GHSA-vfj7-8cjw-p6xm).
  The four Worker lockfiles audit clean.
- paste (RUSTSEC-2024-0436) remains through tikv-jemalloc-ctl and iroh's
  netlink-packet-core 0.8. tikv-jemallocator's unreleased main drops it.

### Baselines (2026-10-19, end of m0)

Cargo.lock: 1,079 packages, 91 names with several versions (110 extra).
Root bun.lock: 660 package names, 72 with several versions (81 extra),
counted per name in its `packages` table; the Worker locks have none. Most
duplicates are ecosystem transitions owned upstream:

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
- Our own crates: web-transport-iroh 0.8.3 holds kio 0.5 (main already takes
  0.6, waiting on a moq-dev/web-transport release), moq-vaapi holds bindgen
  0.70, libloading 0.8, and thiserror 1, and mp4-atom holds thiserror 1.
- Bun has no dedupe, so same-major duplicates (`@babel/types`,
  `@vue/shared`, `minimatch`, `picomatch`, `yaml`) persist until a parent
  re-resolves.

### Kept on purpose

- tracing-wasm: its repository is gone, but the maintained `wasm-tracing`
  fork added 2.9 KB gzipped to `moq_wasm.wasm` for the same behavior.
- tracing-android: `tracing-logcat` would add rustix 0.38.
- humantime-serde, hex, send_wrapper, byteorder: unreleased for years but
  complete, with no advisory.
- assert_matches, cpu-time, stats_alloc: moq-quic stays close to quinn.
- unsafe-libopus: the [Opus backend](/quest/m2/audio-opus-backend.md) quest
  owns the comparison.

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
