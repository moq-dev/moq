# [M] Feature review before the cut

## Goal

Every major feature or work line that landed since the last release, with or
without a new API, has been judged end to end: whether it earns its
complexity, holds its stated contract, is tested and documented, and should
ship as is. Each line that needs work has a follow-up quest that lands before
the cut, or a recorded decision to ship it as is.

What remains is the delta pass: the work that landed after the first pass's
base (`ce616b52c`, 2026-11-14), m2's included, and the lines the first pass
covered only lightly.

## Plan

Recurs once per API or wire cut (decided 2026-10-19; per breaking cut
2026-11-09). The [release cut](/quest/m3/release/cut.md) re-creates this quest
when the maintainer names the next API or wire cut, so the PR finishing this
run adds no successor.
Record what this run learned in the cut's PR notes or a follow-up quest, so
the next run starts from it.
Only `/quest-delete` ends the recurrence.

Decided 2026-11-12 (maintainer, /quest-audit before the cut): ✅ this run
starts now, on what has landed, with a delta pass after m2 (rejected: one pass
after m2 lands, which puts every finding behind the routes chain instead of
beside it). The cut waits for the delta pass. Judge each line adversarially
(AGENTS.md) and challenge its premise, not only its execution: is the feature
worth supporting at all, and would a simpler mechanism the stack already has
deliver most of it? Then look for simpler designs, code that can be deleted,
tests that cost more than they catch, and claims without evidence. File
follow-ups into m1 or m2, or ask the maintainer to ship as is.

The first pass ([moq-dev/moq#8764](https://github.com/moq-dev/moq/pull/8764),
2026-11-14) covered `d0b086e71` (the `release` merge base, 2026-10-09) to
`ce616b52c`: 3,652 commits, 588 of them `feat`. Its follow-ups and the lines
it would ship as is are listed in that PR. It covered the MPEG-TS work the
first run owed (decided 2026-10-19): the guide's evidence holds (every unit
test `doc/bin/ts.md` cites exists, `ts.yml` runs every harness flag it cites,
and the TS Nightly was green for its last eight runs), and its contract
breaks became m1 quests. Decided 2026-11-14 (maintainer, pick on #8764): ✅
every line the first pass would ship as is ships as is, and its follow-ups
carry the maintainer's picks; the VR catalog fields stay, since a VR demo
([moq-dev/moq#6373](https://github.com/moq-dev/moq/pull/6373)) reads them.

The delta pass:

- Inventory `git log --first-parent ce616b52c..origin/main` by area, and the
  quests deleted in that range, the same way. It includes
  [moq-dev/moq#8741](https://github.com/moq-dev/moq/pull/8741) (byte streams
  and the TCP pump) and [moq-dev/moq#8721](https://github.com/moq-dev/moq/pull/8721)
  (`@moq/obs`), open at the first pass.
- Give a full pass to what the first pass read lightly: the RTMP, RTSP, and
  SRT gateway lifecycles, CLI flag changes, JSON snapshot budgets and
  broadcast-clock timestamps, the codec submission/receive split
  ([moq-dev/moq#8372](https://github.com/moq-dev/moq/pull/8372)) and its drain
  lifecycle, and moqsink's restart and completion lifecycle (three fixes and
  two quarantines in the window).
- Confirm each first-pass decision landed as picked.

What the first pass learned:

- One read-only reviewer per cluster (routing and relay, Rust delivery,
  js/net, transport, auth and crypto, native media, formats and TS, browser
  media, bindings, CI and tests) covers a window this size; a reviewer that
  splits its own cluster into sub-reviewers can stall waiting on them.
- Reviewers' counts and premises need checking before they become quests:
  two claims fell on a re-read (a type said to have about 60 references had
  6; a quest said to be obsolete still had its premise in the code).
- Check first next time: exports and wire fields whose doc comment says
  "nothing reads" or "MAY ignore"; settings that default to infinity with no
  non-test setter; code copied between Rust crates or between Rust and JS
  (TS republish, io_uring and tokio WebTransport, reconnect state, sibling
  binding tests), where fixes land in one copy only; quarantined tests with no
  owning quest; features whose consumer quest later moved to m4 or m5 or
  downstream; and whether a "proof" checks code that ships or only a model of
  it.

## Required

- [m2](/quest/m2/README.md) - the delta pass reviews what m2 lands, so the cut's code is final first
