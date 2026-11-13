# [XS] Review and landing docs match the pipeline

## Goal

`AGENTS.md` and `CONTRIBUTING.md` describe the review and landing pipeline
moq runs today, so agents pick the right local reviewer and stop waiting for a
Check that never runs on PRs into `main`.

## Plan

Decided 2026-11-12 (user, /quest-plan), from moq's trimmed Pullfrog learnings:

- `AGENTS.md:48` says "Pullfrog's online review already supplies the DeepSeek
  pass", but Pullfrog reviews with Claude now. Which local reviewer should the
  corrected line name?
  - ✅ Codex, or DeepSeek through opencode when Codex wrote the change last, so
    every PR gets a non-Claude local review beside Pullfrog's (rejected: Codex,
    else Claude; Codex only).
- `CONTRIBUTING.md:37` says ready PRs into `main` run **Check** on each push.
  Since [moq-dev/moq#8425](https://github.com/moq-dev/moq/pull/8425),
  `.github/workflows/check.yml` runs only on PRs into `release`, and main PRs
  get their verdict from the merge queue. Say so, and that a contributor gets a
  signal before queueing from `just ci`.

Caused by: [moq-dev/moq#8425](https://github.com/moq-dev/moq/pull/8425) for
the Check claim; the Pullfrog model changed in its console, outside the repo.
