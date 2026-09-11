# [L] Thin justfiles with one impact map

## Goal

Every justfile is a menu: each recipe is a single line that runs a tool or a
script under `sh/`, `just --list` shows each recipe once, and no recipe body
is bash. The scoped `check`, `fix`, and `test` resolve the branch diff once,
in one impact map, instead of a scope regex per language justfile plus a
mirror of them in the strict-tools preflight. Nothing that tests the tooling
itself runs on a pull request. A recipe that no workflow, doc, skill, or other
recipe invokes is gone, except the hand-run demo and infra ops recipes, which
stay as they are.

## Plan

Layout:

- New root `sh/`. Root helpers sit at the top (`sh/dispatch.sh`,
  `sh/markdown.sh`, `sh/shell.sh`, `sh/clean.sh`); per-area tooling moves
  under `sh/<area>/`: `rs/scripts` to `sh/rs`, `.github/scripts` to `sh/gh`,
  `go/scripts` to `sh/go`, `kt/scripts` to `sh/kt`, `swift/scripts` to
  `sh/swift`, `dart/scripts` to `sh/dart`, and the OBS recipe bodies to
  `sh/obs`. Workflow references to moved paths are updated mechanically here;
  the next quest replaces them with recipes.
- Package build scripts that nix, CMake, gradle, and the dart build hook
  point at stay put: `rs/*/build.sh`, `rs/moq-gst/{package,scrub,smoke}.sh`,
  `cpp/obs/build.sh`, `infra/*/publish.sh`. The harness under `test/` is
  harness code, not tooling; it stays too.
- `shfmt -f .` already enumerates `sh/`, so the shell lint covers everything
  the justfiles stop containing.

Dispatch:

- `just check [BASE]`, `just fix [BASE]`, and `just test [BASE]` are one line
  each: `sh/dispatch.sh check|fix|test "$BASE"`. The script resolves BASE
  (arg, `GITHUB_BASE_REF`, upstream, `origin/main`), lists changed files into
  a temp file, and applies one impact map: which language modules run, which
  tools `MOQ_STRICT` demands, and whether the root orchestration changed and
  everything runs. The map lives in one place; the `scope` variables in the
  js, py, kt, swift, go, and dart justfiles and the regex table in `_tools`
  are deleted with it.
- Per-language `check`, `fix`, and `test` lose their `$FILES` parameter and
  skip logic. Only rs still receives the list: `just rs check-changed
  <listfile>` (and `fix-changed`, `test-changed`) hand the path to
  `sh/rs/select.sh`, which does the crate selection today spread over
  `_select`, `_names`, and `_wants-wasm`.
- Passing a path instead of the list kills the argv budget:
  `changed_max`, `_changed-cap`, `_changed-test`, `_echo`, and the E2BIG
  commentary go.
- `check-all`, `fix-all`, and `test all` keep their names; cache.yml,
  nightly.yml, and the docs call them.

Delete:

- Self-tests: `_changed-test`, `_markdown-test`, `_select-test`,
  `_doc-names-test`, `_publish-test`, `sh/rs/package-nfpm.test.sh`,
  `sh/gh/package-binary.test.sh`.
- Guards: `_doc-names` and `doc-names.jq`, `_fuzz-lock`, and the publish
  lower-bound check. This is a deliberate trade: the guards were three
  incidents' worth of bash plus their self-tests on every pull request. Where
  each surfaces instead: a doc-directory collision shows up as an intermittent
  `failed to remove directory` in the `cargo doc` step of whichever pull
  request selects both crates, not deterministically (resolve with `doc =
  false` as before); a stale internal lower bound fails release-plz's publish.
  `_fuzz-lock` is already deleted by #3543, which folds the fuzz harness into
  the workspace so the root lockfile and `--locked` cover it; if that lands
  first there is nothing left to remove here. `alert.sh check-coverage`
  stays: it is a lint of alert.yml, not of a recipe.
- The `worktree` recipe and its 170 lines, plus the Worktrees section of
  `test/README.md`. This absorbs the justfile half of
  [Harness drive-bys](/quest/m2/harness-drive-bys.md); keep `_base` folded
  into `sh/dispatch.sh`.
- `rs bump` and `rs semver`. `rs release` stays for release-rs.yml.
- The `mod` lines in `demo/justfile`: `just pub`, `just relay`, `just boy`,
  `just sub`, `just web` are the only spelling, and `--list` stops showing
  each twice. `just demo` remains the default recipe and `just dev` its
  documented alias.

Rename:

- Root `wasm` becomes `js wasm`: it emits `js/wasm/dist`, so it is a JS
  package build. `rs wasm` stays the compile gate and `test wasm` the browser
  run. Update wasm.yml, root `build`, `test/wasm/run.sh` (which runs `just
  wasm` to build the package under test), and the four doc references.

Keep, moved into scripts unchanged in behavior: the remark mirror-and-diff in
`sh/markdown.sh` (remark-cli has no check mode and the lint presets are
wanted), `_shell`, `_flake` as its one-liner, `clean`, the rs `package` and
`fuzz` bodies, and the OBS `compile`, `_includes`, `_unit`, `test`, `check`,
and `preset` bodies.

Docs: `doc/setup/dev.md`, `CONTRIBUTING.md`, `test/README.md`, and the
`CLAUDE.md` mentions of `just wasm` follow the survivors. Verify with `just
check`, `just test`, and `just check-all`, and confirm every recipe name
check.yml, cache.yml, nightly.yml, smoke.yml, wasm.yml, obs.yml, swift.yml,
and release-*.yml invoke still resolves.

## Related

- [Harness drive-bys](/quest/m2/harness-drive-bys.md) - the relay `kind` field decision that stays behind once the `worktree` recipe is deleted here
