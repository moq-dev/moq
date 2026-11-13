# [L] API shape review before the cut

## Goal

Every public API added or changed since the last release, in Rust, JS, the
FFI, and every binding, has been reviewed for a better shape, and each change
the maintainer approves is a quest that lands before the cut.

## Plan

Recurs once per API or wire cut (decided 2026-10-19; per breaking cut
2026-11-09). The [release cut](/quest/m3/release/cut.md) re-creates this quest
when the maintainer names the next API or wire cut, so the PR finishing this
run adds no successor.
Record what this run learned (inventory method, findings, what to check first)
in the cut's PR notes or a follow-up quest, so the next run starts from it.
Only `/quest-delete` ends the recurrence.

Starts once m2 lands, so the APIs it reviews are the ones that ship; the cut
waits for it. API shape is the maintainer's call (decided 2026-10-19), so this
run changes no code. Inventory the diff against the last published version
by running `just api-diff`
locally against `main`, since its CI report only appears on the cut PR, and
hand diff the surfaces it cannot see. For each export that is awkward, judge it
against AGENTS.md's Public API rules: names mirrored across languages, no
callbacks or 4+ args, misuse unrepresentable, private until a consumer needs
it. Propose 2-3 shapes with a recommendation, ask the maintainer, and file
each approved change as a quest in m2, which holds the cut until it lands.
Model items only the built-in sessions use stay private
([moq-dev/moq#6747](https://github.com/moq-dev/moq/pull/6747)); this review judges the shape of what ships.

This run's first pass, ahead of m2 (2026-11-07, maintainer, /quest-plan):
an audit of `release` (a005c7ad9a) against `main` (a9d7925123) plus the open
quests. `just api-diff` gave the mechanical inventory; nine read-only
reviewers (net model, net session, transport, media formats, native media,
apps, JS core, browser media, bindings) scored 127 clusters out of 10 for
performance, usability, testability, and future fit, and searched for
consumers in this repository and its downstream applications. Mean 6.9; three
clusters were restrictive (the native playout clock, JS duration units, and
moq-video's environment knobs). Its approved changes are m2 quests
(route-roles, js-time-types, mux-sync, no-sentinels, video-devices,
gateway-ingest (landed), and the api-shape epic (landed)),
two m1 quests, and plan edits. Check first, next run: surfaces `just
api-diff` cannot see (GStreamer properties, CLI flags and env, MIGRATE drift)
and publish versions of packages exempt from the cut's version. `just
api-diff` compares against this repository's `release` branch, which never
held `@moq/qmux`, so compare `js/qmux`'s candidate API against npm's newest
`@moq/qmux` (0.3.4 today, from moq-dev/web-transport) by hand, and the Rust `qmux`
crate against crates.io's newest `qmux` (0.6.6 today) the same way. Run
this comparison only after both web-transport publishers are frozen
([qmux-js-retire](/quest/m3/release/qmux-js-retire.md),
[qmux-rs-retire](/quest/m3/release/qmux-rs-retire.md)), so the newest is
the last; if a freeze lands later, repeat it.
This review still runs after m2 lands, over what changed since.

## Required

- [Breaking release readiness](/quest/m2/README.md) - the release's code is final before it is reviewed
