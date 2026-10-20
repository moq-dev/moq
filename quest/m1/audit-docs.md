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

Follow `doc/AGENTS.md` for what belongs in `doc/`. The m0 run (2026-10-19)
fixed stale codec, driver, auth, snapshot, Go wrapper, and stats-example
claims, documented every public item in hang, moq-archive, moq-auth,
moq-relay, and moq-nvenc, and filled the core JS packages' gaps. Start by
comparing against these baselines:

- Rust: 26 crates set `#![warn(missing_docs)]`. Count the rest with
  `cargo rustdoc -p <crate> --lib --all-features -- -W missing_docs`; only
  `--all-features` reaches feature-gated items, which is how the m0 run found
  moq-auth's undocumented server refusals, and `just rs wasm` reaches
  wasm-only items such as kio's `MaybeSend`. `moq-mux`, `moq-ffi`, and `moq-c`
  have follow-ups. `moq-video`, `moq-audio`, and `moq-sock` are documented on
  Linux but stay unlinted: they gate public items on macOS, Windows, and
  Android, and the dev shell has no std for those targets, so a lint could
  only be verified in the merge queue's platform jobs. `moq-gst` cannot lint
  because `gst::plugin_define!` emits undocumented public items. `moq-v4l`
  is a fork of the `v4l` crate with 222 undocumented hand-written items beside
  its generated `sys` bindings; decide whether a fork earns docs before
  writing them. `libmoq` and the `moq-native` tombstone export nothing.
- JS: no tool checks doc comments; the [follow-up](/quest/m1/docs-js.md) adds
  one. Until it lands, walk each published package's `exports` with the
  TypeScript compiler API. The m0 baseline was 608 exports with 84
  undocumented and 676 public class members with 159 undocumented, nearly all
  in `@moq/publish`, `@moq/watch`, and `@moq/boy`.
- Links: GET (not HEAD) every external URL in `doc/` with a browser user
  agent; 262 answered except npmjs.com package pages, which refuse
  non-browser clients (check `registry.npmjs.org/<package>` instead), and
  placeholder hosts. crates.io answers 200 to such a GET.
- CLI: all complete shell examples parse with `--help` appended against a
  build with `capture,play,transcode,iroh,openh264`, which unknown flags fail
  but bad values do not. `gst-inspect-1.0` of a debug `moq-gst` build lists
  every element and pad property `doc/bin/gstreamer.md` names.
- Cross-Package Sync: walked from the 2026-10-16 release cut (`d0b086e71b`)
  through `9b90cdd569`. Walk the next run from there. The table's Go row
  and `rs/moq-ffi/AGENTS.md` name `go/wrapper/moq`, which does not exist;
  the wrapper is `go/wrapper/*.go` and its subpackages.
