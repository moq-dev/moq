# [M] rust-js spike

## Goal

A report on running [rust-js](https://github.com/rust-js-lang/rust-js) over
moq-net's lite codec and model layer, and the maintainer's choice of the
translator's foundation from it: Charon in a new repo (the default), an own
THIR driver, contributing to rust-js, or forking it. The PR rewrites
[the rs2ts repo quest](/quest/m1/rs2ts/repo.md) for the chosen foundation.

## Plan

A planning quest: the report and the rewritten quests are the output, not
production code. Keep spike code on a scratch branch or in the report's
artifact. It reports facts; no pass/fail bar is set in advance.

Report, for the codec (`coding/` plus the lite message types) and the model
(`model/` plus kio):

- What translates unmodified, what fails, and the source changes needed,
  per construct: close-on-last-drop through `Arc` (and `Arc::strong_count`
  logic in `model/resume.rs`), `Weak`, kio's wakers and RAII guards,
  atomics, `BTreeMap::range`, maps keyed by `u64` or structs, the nested
  `Option`s, `Arc<dyn Expiry>`, and the `unsafe` frame storage.
- Gzip size and per-frame CPU of the generated codec against js/net's
  hand-written one. Bun or node numbers are a proxy for the browser; label
  them so.
- What it takes to get the shapes the line decided: `U64` as two `u32`
  halves instead of BigInt, checked integer arithmetic instead of wrapping,
  TypeScript output, and no async translation. Say whether each is a config
  option, an upstream patch, or a fork.
- rust-js's churn, maintenance, and license, and Charon's for comparison:
  Charon has no crates.io release, and its LLBC format breaks about five
  times a week.

The prior Charon evidence is the 2026-09-27 spike and the draft prototype in
[#5049](https://github.com/moq-dev/moq/pull/5049). Do not post on rust-js
without the maintainer's approval; it is outside our orgs.

Public API: none. Wire: none.
