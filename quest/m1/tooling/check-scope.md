# [S] check.yml scopes from the impact map

## Goal

`.github/workflows/check.yml` decides whether a PR needs the build and test jobs from `sh/dispatch.sh`, the line's single impact map. The two inline "Scope" steps that grep the diff for quest-only and doc-only changes go away, so there is one place that maps changed paths to work.

## Plan

Main's #4407 added the inline Scope steps (a quest, `.claude/`, or root-markdown-only diff skips the build) after this line had already moved CI scoping into `sh/dispatch.sh`. Teach the impact map that those paths need no build, have the Scope steps call it (or drop them if the dispatcher already makes the jobs no-ops), and keep the skipped-but-required checks reporting success so branch protection still passes on quest-only PRs. `just gh check` must keep enforcing that workflow steps run recipes.
