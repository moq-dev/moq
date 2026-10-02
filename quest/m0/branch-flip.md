# [M] Trunk is main, releases ship from release

## Goal

`dev` is renamed `main` and is the default branch: the trunk, where breaking
changes are allowed and every quest and PR lands. Today's `main` is renamed
`release`, a permanent branch where release-plz and all publishing run. Code
reaches `release` when a release is cut, by merging `main` in; urgent fixes in
between are cherry-picked. AGENTS, CONTRIBUTING, and the quest tree describe
that model, with no `dev` left.

## Plan

Facts (2026-10-02):

- Publishing on push: `release-rs` (release-plz), `release-js`, `release-py`,
  and `release-go` trigger on `main`; `release-kt-lib` and `release-swift-lib`
  on `main` and `dev`. Everything else publishes from tags and is unaffected.
- `cache.yml` and `swift.yml` warm caches on `main` on both branches. PRs
  read caches only from the default branch, so writers stay on trunk and
  need no change. `obs`, `platform`, and `quest` push filters name `main` and
  `dev`. Nightly and interop crons run on the default branch.
- Rulesets: "main" targets `~DEFAULT_BRANCH` (linear history, Check and Test
  required), so it moves to trunk. "dev" targets `refs/heads/dev` (PRs only,
  squash or merge commit) and does not follow a rename.
- Renaming `main` makes GitHub retarget its ~35 open PRs to `release`; `dev`'s 10
  follow it to the new `main`.
- `sh/changed.sh` falls back to `origin/main`, which stays right for trunk.
  `quest` hardcodes `main` as the milestone base, which becomes right too.
- About 50 quest files on `main` (40 on `dev`) say "on `dev`"; AGENTS.md (API,
  Development, quest sections), CONTRIBUTING.md, `py/AGENTS.md`, and
  `quest/README.md` encode the split.
- `nix run github:moq-dev/moq`, `npx skills add moq-dev/moq`, and docs "edit"
  links follow the default branch.

Decided (2026-10-02):

- ✅ Flip, over keeping `dev` with continuous sync or dropping a stable branch.
- ✅ A release is cut by merging `main` into `release` (merge commit), by
  hand. Rejected: resetting `release` to `main`, and backport-only flow.
- ✅ Backports are manual cherry-pick PRs into `release`; automate later if
  it hurts.
- ✅ After each publish, a workflow merges `release` back into `main`, so trunk
  carries the published versions and CHANGELOGs. Rejected: running the
  release-plz PR on `main` and publishing from `release`.
- ✅ Trunk is the default branch. User-facing installs in docs pin `release`
  (`github:moq-dev/moq/release`); all branch-triggered publishing, including
  kt and swift libs, runs only from `release`. Nightly and edit links stay on
  trunk.
- ✅ A new `release` ruleset (no deletion or force push, Check and Test
  required, PRs only, merge commits allowed); the `dev` ruleset is deleted.
- ✅ Open PRs that land on `release` through the rename are retargeted to the
  new `main`, except any explicitly meant as backports.
- ✅ One mechanical wording sweep: "on `dev`" goes from quests (trunk allows
  breaks), the rules describe trunk, `release`, and backports, and
  `dev`-only process quests are deleted.
- ✅ One quest, m0, right after dev-sync lands.

Sequence:

1. [dev-sync](/quest/m1/dev-sync.md) (#4720) lands.
2. A PR on `dev`: the wording sweep, trunk stops publishing, `obs`,
   `platform`, and `quest` push filters name `main` and `release`, the
   back-merge workflow, docs pin `release`.
3. A small PR on `main`: publish triggers move to `release`; cache writers
   keep `main`, so they stop running there.
4. Admin, back to back, run by the agent only after the maintainer's
   go-ahead in chat: rename `main` to `release`, rename `dev` to `main`, set
   the default branch, create the `release` ruleset, delete the `dev`
   ruleset, retarget open PRs. Dry-run first and show the PR list.
5. Verify: a no-op push to `main` publishes nothing; release-plz runs on
   `release`; the back-merge opens on the first publish.

Public API: none. Wire: none. Contributors see the new branch model.
