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

Follow `doc/AGENTS.md` for what belongs in `doc/`. The docs site deploys from
`release`, so a page describing an unreleased package (such as `@moq/video`,
first published by the release cut) is expected to 404 on its registry until
then. The m1 run (2026-10-21) found the stale claims in pages no PR had touched
for a while: the root `README.md` crate table, `doc/concept/transport.md`, and
the codec lists in `doc/bin/rtmp.md` and `srt.md`. Read those kinds of pages
claim by claim against the code first; the pages recent PRs edit are the
freshest. Baselines:

- Rust: 31 crates set `#![warn(missing_docs)]`, including `moq-mux`,
  `moq-ffi`, and `moq-c` since
  [#6088](https://github.com/moq-dev/moq/pull/6088) and
  [#6091](https://github.com/moq-dev/moq/pull/6091); m1 added `moq-shaper`.
  Count the rest with
  `cargo rustdoc -p <crate> --lib --all-features -- -W missing_docs`.
  `moq-auth` is ambiguous with its crates.io copy, so pass its
  `path+file://` spec. Every linted crate was clean with all features.
  `moq-audio`, `moq-video`, and `moq-sock` were also clean on Linux but stay
  unlinted for their macOS, Windows, and Android items. `moq-gst` has 3 items
  from `gst::plugin_define!`. `moq-v4l` reports 3108, nearly all its generated
  `sys` bindings; whether the fork earns docs is still undecided.
- JS: the doc build renders the API reference with TypeDoc and fails on any
  TypeDoc warning, such as an unresolved link, but not on an undocumented
  export, so look for those and check that docs say something.
- Links: GET (not HEAD) every external URL in `doc/` with a browser user
  agent. 316 URLs: all answered except placeholder and local hosts, relay
  URLs that only speak MoQ, npmjs.com and w3.org pages that refuse
  non-browser clients, and the unreleased items above. Check npm packages on
  `registry.npmjs.org/<package>` and crates on the crates.io API, since
  crates.io pages answer 200 to anything. `README.md`'s `moq-c` badge waits on
  the crate's first publish.
- CLI: all 68 shell invocations in `doc/`, `README.md`, and
  `rs/moq-cli/README.md` parse with `--help` appended against a build with
  `capture,play,transcode,iroh,openh264`, after substituting a URL for the
  elided `moq ...` connect. Unknown flags fail but bad values do not.
  `gst-inspect-1.0` of a debug `moq-gst` build lists every element and pad
  property `doc/bin/gstreamer.md` names.
- Cross-Package Sync: walked from `9b90cdd569` through `7d2a91fe66`. Walk the
  next run from there. At that baseline, Go could not observe first-dial
  failures because `Dial` returned only once connected. Every binding now
  returns a connecting session with one coherent `state()` snapshot; verify
  first-dial failure observation and Kotlin/Dart wording against that API.

## Required

- [Release cut](/quest/m2/release/cut.md) - the release in flight ships before this run starts
