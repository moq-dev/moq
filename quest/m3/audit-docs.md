# [M] Audit the docs

## Goal

Run the `docs` audit: `quest audit docs` prints what it checks, what each finding becomes, and how to plan its next run.

## Plan

Baseline: 2026-11-23 at `890ea4e531` (before the 0.22.0 cut). Previous run: [moq-dev/moq#6170](https://github.com/moq-dev/moq/pull/6170). Follow `doc/AGENTS.md` for what belongs in `doc/`. The docs site deploys from `release`, so pages and registry links for packages first published at 0.22 (`@moq/clock`, `@moq/video`, `doc.moq.dev/bin/inspect`, `docs.rs/moq-hls/.../struct.Include.html`) 404 until 0.22 publishes.

### Check first

- Stale claims: walk commits since `890ea4e531` and pages no recent PR touched (`js/msf/README.md`, `rs/moq-loc/README.md`, `rs/moq-msf/README.md`, `doc/concept/use-case/distribution.md`, `doc/setup/troubleshooting.md`). After 0.22 finalizes `moq-lite-07`, verify no doc still says `moq-lite-07-wip` is opt-in.
- Rust missing docs: 8 published crates lacked `#![warn(missing_docs)]` at `890ea4e531`: `moq-audio`, `moq-congestion`, `moq-gst`, `moq-sock`, `moq-v4l`, `moq-vaapi`, `moq-video`, `qmux`. Count undocumented items with `cargo rustdoc -p <crate> --lib --all-features -- -W missing_docs`. Whether `moq-v4l`'s generated `sys` bindings earn docs is still undecided.
- JS docs: TypeDoc fails on warnings but not on undocumented exports; check exported symbols in `js/*/src/` beyond index re-exports.
- Links: GET (not HEAD) every external URL in `doc/` and `README.md` with a browser User-Agent; verify crates on `crates.io/api/v1/crates/<name>` and npm packages on `registry.npmjs.org/<pkg>`, especially the newly published 0.22 pages and packages once the cut lands.
- CLI and GStreamer: verify shell invocations in `doc/`, `README.md`, and `rs/moq-cli/README.md` parse with `--help` appended, and that `doc/bin/gstreamer.md` properties match `rs/moq-gst` (all matched at `890ea4e531`).
- Cross-Package Sync: check commits since `890ea4e531` for features one language or binding lacks.

## Audit

- docs

## Scheduled

- 2026-11-30 - one week after the 2026-11-23 pre-0.22 audit run

## Required

- [Release cut](/quest/m3/release/cut.md) - the release in flight ships before this run starts
