# [M] Documentation audit

## Goal

Docs match the code: stale pages, broken examples, broken links, and
undocumented exported symbols are fixed or planned.

## Plan

Recurs each major release: the PR that finishes a run keeps its results in
its own body and re-adds this file at the end of m3's `Required`, behind the
[release cut](/quest/m3/release/cut.md), with Check first rewritten. Fixes land
in that PR; only maintainer decisions (public API, scope) and changes too
large to review alongside become follow-ups.

Follow `doc/AGENTS.md` for what belongs in `doc/`. The docs site deploys from
`release`, so a page for a package the cut first publishes 404s on its
registry until then.

Previous run: [moq-dev/moq#6170](https://github.com/moq-dev/moq/pull/6170).

### Check first

- Pages no recent PR touched, claim by claim against the code: last run's
  stale claims were in the root `README.md` crate table,
  `doc/concept/transport.md`, and the codec lists in `doc/bin/`.
- Rust: count undocumented items with `cargo rustdoc -p <crate> --lib
  --all-features -- -W missing_docs` in crates without
  `#![warn(missing_docs)]`. `moq-auth` needs its `path+file://` spec. Whether
  `moq-v4l`'s generated `sys` bindings earn docs is still undecided.
- JS: TypeDoc fails on warnings but not on an undocumented export; look for
  those.
- Links: GET (not HEAD) every external URL in `doc/` with a browser user
  agent. Check npm packages on `registry.npmjs.org` and crates on the
  crates.io API, since crates.io pages answer 200 to anything.
- CLI: every shell invocation in `doc/`, `README.md`, and
  `rs/moq-cli/README.md` parses with `--help` appended; `gst-inspect-1.0`
  lists every property `doc/bin/gstreamer.md` names.
- Cross-Package Sync: walk the commits since `7d2a91fe66` for a feature one
  language or binding lacks.

## Required

- [Release cut](/quest/m3/release/cut.md) - the release in flight ships before this run starts
