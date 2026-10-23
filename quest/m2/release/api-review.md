# [L] API shape review before the cut

## Goal

Every public API added or changed since the last release, in Rust, JS, the
FFI, and every binding, has been reviewed for a better shape, and each change
the maintainer approves is a quest that lands before the cut.

## Plan

Recurs once per major release (decided 2026-10-19). The [release
cut](/quest/m2/release/cut.md) re-creates this quest for the next release when
it shifts the milestones, so the PR finishing this run adds no successor.
Record what this run learned (inventory method, findings, what to check first)
in the cut's PR notes or a follow-up quest, so the next run starts from it.
Only `/quest-delete` ends the recurrence.

Starts once m1 lands, so the APIs it reviews are the ones that ship; the cut
waits for it. API shape is the maintainer's call (decided 2026-10-19), so this
run changes no code. Inventory the diff against the last published version
by running `just api-diff`
locally against `main`, since its CI report only appears on the cut PR, and
hand diff the surfaces it cannot see. For each export that is awkward, judge it
against AGENTS.md's Public API rules: names mirrored across languages, no
callbacks or 4+ args, misuse unrepresentable, private until a consumer needs
it. Propose 2-3 shapes with a recommendation, ask the maintainer, and file
each approved change as a quest in m1, which holds the cut until it lands.
Related: [session-only items stay private](/quest/m1/api-audit/model-private.md), which decides what
moq-net exposes; this review judges the shape of what ships.

## Required

- [Breaking release readiness](/quest/m1/README.md) - the release's code is final before it is reviewed
