# [M] Feature review before the cut

## Goal

Every major feature or work line that landed since the last release, with or
without a new API, has been judged end to end: whether it earns its
complexity, holds its stated contract, is tested and documented, and should
ship as is. Each line that needs work has a follow-up quest that lands before
the cut, or a recorded decision to ship it as is.

What remains is the delta pass: the work that landed after the 2026-11-16
pre-cut audit's base (`6ac78b68ee`), m2's included.

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

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit): ✅ the
2026-11-14 pick is reopened. The maintainer asked for an exhaustive audit of
the same window before the 0.22 announcement, with every addition judged for
a long-term purpose and a caller, and the lite-07 draft audited for what it
could lose. It covered `d0b086e71` to `6ac78b68ee` (about 3,800 commits):
a digest of moq.pro's and Kixel's roadmaps, fifteen read-only reviewers, and
a 15-round interview. Its decisions are m2 quests, listed in
[m2's Plan](/quest/m2/README.md) under the same date. Of the lines the
first pass would ship as is, some left (YIELDED continuations, socket
endpoints, video `warmup`, the vendored WebRTC stack, the unused exports)
and more were renamed or reshaped.

The delta pass now starts from `6ac78b68ee`, and its second bullet below is
done: the gateway lifecycles, CLI flags, JSON snapshots, the codec split,
and moqsink were read in full by that audit.

What the 2026-11-16 audit learned, beyond the first pass's list below:

- Give every reviewer the downstream roadmaps as a digest, with what each
  project stopped needing. "Nobody planned, anywhere" was the strongest
  finding, and "a named consumer needs it" the strongest defence.
- Ask what a feature costs when unused. Several of the largest findings
  were costs everyone pays for a feature few use: a record before every
  frame on a relay link, memory per connection, and a pause that depends on
  a signal browsers drop.
- A feature whose other half moved past the cut does not freeze alone:
  defer both halves together (YIELDED continuations and takeover).
- Check a PR's state when the question is asked, not when the inventory was
  taken: the audit asked about a PR as open ten hours after it merged.
- Reviewers' counts still need a re-run before they become quests: three
  more fell (110 sites that were 108, 127 that were 3, 119 that were 185).

The delta pass:

- Inventory `git log --first-parent 6ac78b68ee..origin/main` by area, and the
  quests deleted in that range, the same way (byte streams in `#8741` already
  left moq for Kixel, and the gateway lifecycles, CLI flags, JSON snapshots,
  codec split, and moqsink were covered in full by the 2026-11-16 audit).
- Confirm each 2026-11-14 and 2026-11-16 decision landed as picked.

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
