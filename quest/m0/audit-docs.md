# [M] Documentation audit

## Goal

Docs match the code: stale pages, broken examples, broken links, and
undocumented exported symbols are fixed or planned.

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

Follow `doc/AGENTS.md` for what belongs in `doc/`. Starting points
(2026-10-19): check every CLI example against `--help`, and walk the
Cross-Package Sync table in AGENTS.md for changes merged since the previous
run that skipped a target. `#![warn(missing_docs)]` is set in 12 of 42
crates; find the published ones without it. VitePress
checks internal links only; nothing checks external ones (try `lychee`).

Audit checkpoint (2026-10-19, base `82c2d5dde`):

- Corrected TS Opus and FLV/RTMP codec support, including the stale claim that
  MP3 is dropped. Corrected Rust browser-driver setup, the net type example,
  and the spawned connection lifecycle. Corrected JSON/flate snapshot docs:
  untimed groups are subject to wall-clock staleness too.
- Replaced the production recipe's retired `[auth].key` with an auth-server
  URL and the `moq auth serve --key` process it needs.
- Checked 259 distinct external URLs from 69 documentation pages: 217
  answered successfully. Verified unavailable RTSP/ICE, RTC wire, C, Python,
  and Swift references and replaced them with current repository source.
  Crates.io's HTML routes return 404 to this checker even for existing
  packages, whose API routes work; protocol endpoints and example hosts are
  not website health checks. Sixteen sites refused HEAD with 403 and still
  need manual verification. Every GitHub `main` source path in the pages exists.
- Built the default-feature `moq` binary and checked 34 help contexts.
  All 37 complete, supported shell examples parsed with `--help`; seven
  examples need optional capture/play/transcode/iroh features. Gateway and
  inspection pages use supported flags. Feature-only flags were checked
  against their declarations, but those builds and GStreamer inspection
  remain to be exercised.
- The stats documentation validator, `quest check` (438 documents), and
  `git diff --check` passed, and so did `just check` on the changed packages
  (the doc site and the JS packages).
- The exported-symbol audit is incomplete. Of 38 Rust library roots, 12
  enable `missing_docs`; 24 publishable roots do not: hang, kio, libmoq,
  moq-archive, moq-audio, moq-auth, moq-c, moq-clock, moq-ffi, moq-flate,
  moq-gst, moq-json, moq-loc, moq-msf, moq-mux, moq-native, moq-nvenc,
  moq-relay, moq-sock, moq-stats, moq-transcode, moq-uring, moq-v4l, and
  moq-video. This is lint coverage, not a claim that each has missing docs.
  Run rustdoc with missing-doc diagnostics, review public TypeScript exports,
  and finish the Cross-Package Sync walk before calling this run complete.
  Do not add the successor until that work and the remaining checks pass.
