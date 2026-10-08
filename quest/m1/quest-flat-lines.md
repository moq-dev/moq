# [M] moq adopts flat questlines from the current quest CLI

## Goal

moq pins a quest release where questlines are planning groups with no branch
of their own: every quest, including a questline README's remaining work, is
its own PR against `main`. The child work already merged into questline
branches reaches `main`, and those branches retire.

## Plan

kixelated/quest#44 dropped questline branches, `quest branch`, and
`quest ready --remote`; kixelated/quest#51 changed the import skill to treat a
report's fix as a claim and plan each issue fully. moq pins kixelated/quest
`677f8d1`, which includes both; `quest check` passed on the tree unchanged.

Decided 2026-10-04:

- A quest of its own, not a routine pin bump, because questline branches
  carried merged child work that is not on `main`.
- Goal confirmed: questlines become planning groups whose children PR straight
  to `main`. Finishing any line's remaining children is a non-goal.
- Bump the CLI now, not after the lines land, because the point is to get rid
  of line branches.
- Each code-carrying line branch gets `main` merged in and its umbrella PR
  marked ready; the maintainer lands them later with `/quest-complete`. Merging
  `main` in is less churn than re-cutting each child, and finishing a line
  first would keep its branch alive.
- Code-free lines (quic #3975, cluster-routing #4654, broadcast-epoch #4805)
  are closed and their branches deleted; their children PR to `main`. Nothing
  on them needs landing.
- Nested lines (rs2ts/sans-io #4438, archive/track-timeline #4255) fold into
  their parent branch, then close, so each tree lands through one PR.
- The quest CLI's PR-adoption skill is installed as `quest-iterate` and
  replaces the `takeover` skill, so one skill covers it. kixelated/quest#52
  renamed it upstream to match.
- `CONTRIBUTING.md` drops ", a questline" so the merge-commit rule names only
  `release`.

Interview paper trail (✅ marks the choice):

- Goal confirm: ✅ Confirm / Also finish lines / Bump only.
- Landing default: Land partial now / Re-cut per child / Finish then land /
  ✅ (user) "merge main into the branch, mark PR as ready for review, then I'll
  do quest-complete later".
- Split: Per-line quests / One quest / Batch by size / ✅ (user) "spawn
  sub-agents to merge main for all of them, then mark as ready".
- Order: Bump last / Bump first / Bump first, sync quest files / ✅ (user) "the
  idea is to get rid of line branches", then confirmed ✅ Bump now.
- Empty lines: ✅ Close and retarget / Merge main and ready.
- Nested: ✅ Fold into parent / Retarget to main / Merge main, keep nesting.
- Takeover: Add stub, replace takeover / Add stub, keep both / Skip / ✅ (user)
  "add stub, replace takeover, rename it to quest-iterate".
- CONTRIBUTING: ✅ Yes, edit it / Leave it.

Remaining:

- Land each line's umbrella PR with `/quest-complete`, then delete its branch.
  Landed so far: #4403 wildcard, #4034 archive (with #4255 folded in), #4080
  obs-moq-video, #4162 audio-jitter-target, #4180 transport-upgrade, #4437
  rs2ts (with #4438 folded in), #4640 tstd, and #4653 test-flakes-2. As of
  2026-10-08 four lines remain, and all four conflict with `main` and need
  it merged in again.
  - #4039 auth: its `wip-version.md` child landed on the line (#5004). The
    line's UNAUTHORIZED stream code collides with `main`'s NOT_FETCHABLE
    (0x3A) and moves to 0x3B before it lands.
  - #4079 cpp: the generated-newline fork tags exist
    (`v0.11.0-kixelated.3` and `.4`); bump the line's `uniffi-bindgen-cpp`
    pin from `.2`, then check `OBS (macOS)`.
  - #4519 ffi-shape waits on
    [Bindings](/quest/m0/broadcast-epoch/bindings.md).
  - #4133 qos: lag-splice landed on the line (#5009). Its egress `lag`
    histogram splits into its own quest requiring stats-split (decided
    2026-10-08); follow the qos README for what the line lands without it.

Done when `flake.nix` pins the new quest, no `quest/*README` branch remains,
and `quest check` passes.
