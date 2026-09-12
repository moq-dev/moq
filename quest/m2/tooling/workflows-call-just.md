# [M] Every workflow step runs a recipe

## Goal

No `run:` line in `.github/workflows` names a `.sh` file. Every script a
workflow needs has a `just` recipe, so the menu is the whole command surface
and a script can move or change its arguments without touching twenty
workflows. The same recipes work locally for a dry run.

## Plan

Today the workflows call scripts directly at roughly ninety sites:
`sh/gh/release.sh` (54 calls as a helper library: `parse-version`,
`prev-tag`, `create`, registry existence checks), `sh/rs/package-nfpm.sh`,
`sh/rs/package-binary.sh`, `sh/rs/package-windows.sh`,
`sh/gh/trigger-repo-publish.sh`, `sh/gh/render-formula.sh`, `sh/gh/alert.sh`,
the swift `package`, `package-ffi`, `publish`, `publish-ffi`, `verify`,
`verify-ffi`, and `check` scripts, the go `package-ffi`, `package-wrapper`,
`publish-ffi`, `publish-wrapper` scripts, `sh/dart/package.sh`,
`sh/kt/package.sh`, and `infra/*/publish.sh`.

- Add pass-through recipes where none exist, named by role under the area
  module: `just gh release <subcommand> ...`, `just gh alert ...`,
  `just gh formula ...`, `just gh trigger-repo-publish ...`,
  `just rs package-binary ...`, `just rs package-nfpm ...`,
  `just rs package-windows ...`, `just swift publish|verify|package-ffi|...`,
  `just go publish-ffi|publish-wrapper`, `just dart package`,
  `just infra apt publish` and `just infra rpm publish` (the recipes exist).
  Existing wrappers (`kt package`, `swift package`, `go package-ffi`,
  `go package-wrapper`, `py package`) are used as they are.
- Rewrite every `run:` step to the recipe. Where a workflow runs outside the
  nix dev shell (Windows and macOS runners, AlmaLinux containers), `just` is
  already installed or installable the way nightly.yml does it; keep that
  pattern rather than falling back to a path.
- Record the rule in the CI section of `CONTRIBUTING.md` in one line, and
  have `just gh check` enforce it: fail on any workflow `run:` line matching
  `\.sh\b`.
- Verify with `actionlint` via `just gh check`, and with a manual
  `workflow_dispatch` of apt-repo.yml, rpm-repo.yml, and release-winget.yml
  where the inputs allow a dry run. The tag-triggered release workflows are
  verified by the next release; say so in the PR.

## Required

- [Thin justfiles](/quest/m2/tooling/justfiles.md) - establishes `sh/` and the module recipes the workflows will call
