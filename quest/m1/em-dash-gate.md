# [S] Gate the no-em-dash rule

## Goal

`just check` and the landing gate fail on an em dash in any tracked text file
except the lockfiles, and the PR removes the current ones (213 in 50
files, most in `js/qmux`, `rs/qmux`, and the UI stats panels), so `main`
passes.

## Plan

`AGENTS.md` says "No em dashes", but nothing enforces it. Adopted from
t3code, which turns its agent rules into lint rules, alongside moq.pro's
[matching gate](https://github.com/moq-dev/moq.pro/blob/main/quest/m1/rule-gate.md).

- A `git grep` over the whole tree, run as a check beside
  `check:large_files` in `sh/dispatch.sh`, on every diff. It costs
  milliseconds and still catches an em dash that arrives through a merge.
- No allowlist: docs, drafts, tests, and UI copy are all covered. The fix is
  a hyphen, comma, colon, or a new sentence. A UI empty-value placeholder
  in the stats panels becomes a plain `-`.
- Rust's `allow_attributes_without_reason` already runs here, so the moq.pro
  quest's other half has nothing to add upstream.

Decided with the maintainer on 2026-10-31, in moq.pro's planning session:

- An upstream gate: ✅ plan one in moq-dev/moq / keep the plan to moq.pro.
- Scope: ✅ all tracked text except lockfiles, the same scope as moq.pro.
- Milestone: ✅ m1 / m2.
