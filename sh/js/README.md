# JavaScript checks

`just check` and `just ci check|test` pass the dispatcher’s JS-impacting paths to
`just js check-changed|test-changed`. The selector reads the root Bun workspace
list and each package manifest, then selects changed packages and their
transitive dependents across runtime, development, peer and optional dependencies.
Deleted files still belong to their package directory. Package names and edges
are never maintained separately from these manifests.

An input outside every current workspace selects all packages, including shared
inputs such as `js/common/`, `js/justfile`, `test/tsconfig.json`, `drafts/`, the root
manifest and lockfile. Unrelated paths are filtered by the dispatcher first.
`just js check`, `just js test` and `just js build` explicitly select all packages.

Checks and builds generate WASM only when a selected package or one of its
transitive dependencies is `@moq/wasm`. Harness type-checking uses the shared test
configuration, excluding unselected workspace packages. Harnesses outside every
workspace remain checked because they have no dependency metadata; the all-package
fallback keeps the whole harness check. Shared dependency validation, environment scanning and
common helper tests continue to run. The CI setup query still uses the common
module map, without requiring a JS toolchain before setup.

Run `python3 sh/js/workspace.test.py` for graph and execution regressions. The same
suite runs in `just gh test` nightly. To inspect work without executing it, pass
a file containing relevant paths, one per line, to
`python3 sh/js/workspace.py plan LISTFILE`, or pass `--all`.
