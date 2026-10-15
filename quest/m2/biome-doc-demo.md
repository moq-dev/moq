# [XS] Biome checks doc/ and demo/

## Goal

`just check`, `just fix`, and CI run Biome over the TypeScript, Vue, and
config files under `doc/` and `demo/`, as #5589 did for `test/`, and both
trees pass.

## Plan

#5589 widened the Biome paths in `sh/js/workspace.py` and `js/justfile`
to `test/`. `doc/` and `demo/` are outside them, although the dispatcher's
`js` scope (`sh/dispatch.sh`) already selects JS checks for changes there.

Measured on main at `de6c27302` with `bun biome check doc demo`:

- One format error in `doc/.vitepress/drafts.ts`, which `just fix` resolves.
- One `noUnusedVariables` warning on `compact` in
  `doc/.vitepress/theme/Community.vue`. It is a false positive: the template
  uses it, and Biome does not read Vue templates. Fix it in `biome.jsonc` for
  `.vue` files (enable template support if the pinned Biome has it, else
  scope the rule off for `.vue`), never by renaming the variable.

`demo/pub` is a Worker with its own lockfile (the dispatcher's `workers`
scope). Biome reads only the root config, so cover it too unless that
breaks; record the reason if it stays out.

Keep the selection test in `sh/js/workspace.test.py` covering the new
paths, and update `sh/js/README.md`.

Public API: none. Wire: none.
