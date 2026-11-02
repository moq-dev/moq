# JavaScript checks

`just check` and `just ci` pass the dispatcher’s JS-impacting paths to
`just js check-changed|test-changed`. The selector reads the root Bun workspace
list and each package manifest, then selects changed packages and their
transitive dependents across runtime, development, peer and optional dependencies.
Only scripts declared by a selected package run; harnesses without scripts still
get the shared type-check. Deleted files still belong to their package directory. Package names and edges
are never maintained separately from these manifests.

Sources can also read another tree's files, like `@moq/hang` grading itself against
the `doc/concept/audio-jitter/` corpus. The selector scans each workspace for string
literals that climb out of it (`../`), and a change to or under such a path, even a
deletion, also selects its readers. A check fails when an existing input is missing
from the dispatcher's static JS pattern, which would otherwise never run the readers.

An input outside every current workspace selects all packages, including shared
inputs such as `js/common/`, `js/justfile`, `test/tsconfig.json`, `drafts/`, Rust's
shared JSON vectors, the root manifest and lockfile, and Biome then checks `js/`,
`test/`, `doc/` and `demo/`, including the `demo/pub` Worker outside the workspace.
Narrow selections lint every selected package directory plus `js/common/`. Biome's
full HTML support counts Vue template reads as uses; its HTML formatter stays opt-in.
Unrelated paths are filtered by the dispatcher first.
`just js check`, `just js test` and `just js build` explicitly select all packages.

Harness type-checking uses the shared test
configuration, excluding unselected workspace packages. Harnesses outside every
workspace remain checked because they have no dependency metadata; the all-package
fallback keeps the whole harness check. Shared dependency validation, environment scanning and
common helper tests continue to run. The CI setup query still uses the common
module map, without requiring a JS toolchain before setup.

The root devShell supplies Chromium and exports its Playwright version. Checks
require every npm `playwright` dependency outside `test/smoke` to pin that exact
version. A root `flake.nix` or `flake.lock` change selects JS checks too: when
nixpkgs moves Playwright, update the reported manifests and regenerate `bun.lock`
with `bun install` in the new shell. Smoke has its own flake and freshness check.

Run `python3 sh/js/workspace.test.py` for graph and execution regressions. The same
suite runs in `just gh test` on every check. To inspect work without executing it, pass
a file containing relevant paths, one per line, to
`python3 sh/js/workspace.py plan LISTFILE`, or pass `--all`.
