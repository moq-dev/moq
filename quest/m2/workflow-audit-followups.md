# [XS] Nightly runs zizmor's online workflow audits

## Goal

Nightly runs zizmor's online audits (`impostor-commit`,
`known-vulnerable-actions`, `ref-confusion`) over `.github/` with a
read-only token and alerts through `alert.yml` on a new finding, beside the
offline zizmor run the merge gate already does.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31 /quest-complete run), from [moq-dev/moq#7317](https://github.com/moq-dev/moq/pull/7317),
which finishes [workflow security linting](/quest/m2/workflow-security.md),
and [moq-dev/moq#7202](https://github.com/moq-dev/moq/pull/7202).
Milestone m2 (user: "use recommended for any CI/flakes").

[moq-dev/moq#7317](https://github.com/moq-dev/moq/pull/7317) runs `zizmor --offline` in `just gh check`; the online audits need
the network and a GitHub token, so they belong in Nightly next to
`cargo deny`, `govulncheck`, and `pip-audit`. Use the job's
`github.token` with `contents: read` only, reuse [moq-dev/moq#7317](https://github.com/moq-dev/moq/pull/7317)'s zizmor config for
accepted findings, and give the run a recipe (`just gh` module) so it runs
locally with `GH_TOKEN` set.

No YAML duplicate-key check is needed. In [moq-dev/moq#7202](https://github.com/moq-dev/moq/pull/7202) git's auto-merge left
three duplicate `environment:` keys in release workflows, which the author
caught by hand. actionlint 1.7.12 (the pinned dev shell's) already fails
`just gh check` on both main and `release` for a duplicated key in a
workflow job and in a local action's `action.yml` (checked 2026-10-31), so
the gate would have refused them. The rest of the tracked YAML is not
workflow code; [Biome YAML](/quest/m2/biome-yaml.md) will parse it.

Public API: none. Wire: none.

## Required

- [Workflow and dependency security linting](/quest/m2/workflow-security.md) - its PR, [moq-dev/moq#7317](https://github.com/moq-dev/moq/pull/7317), adds zizmor and its config
