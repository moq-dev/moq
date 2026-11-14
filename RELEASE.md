# Releases

`main` is the trunk; `release` is what ships, and release-plz and every branch-triggered publish run only there.

After the `0.22.0` cut, which still waits on every break planned for it, each release cycle runs in three phases by kind of change, recorded as `phase` in `version.json`: `additive` work first, then `api` breaks, then `wire` changes.
An API break is what a published consumer adapts to when it rebuilds: the public Rust, JS, or bindings API, CLI flags, config keys, and environment variables.
A wire change is what a deployed peer adapts to without rebuilding: a frozen wire version, the hang catalog JSON, or a stored or exported container format.
The maintainer opens the next phase with a PR that changes `phase`. After the `0.22.0` cut, and after the wire cut that ends each cycle, a PR on `main` sets it to `additive` once the cut has merged into `release` and every package it bumps is published (crates.io, npm, and the `moq-c-v*`/`moq-ffi-v*` GitHub releases): the cut PR itself carries breaks, so its API diff must only report, and the API diff's baselines are those published versions, so an earlier reset fails Nightly's `--deny` on every break the cut shipped.
`main` can be cut on demand in the additive and API phases; in the wire phase it is cut only once its wire version is final, so wire work comes last and stays short.
While `phase` is `additive`, Nightly runs `just api-diff --deny` on `main` and the cut PR's API diff denies too, so a break that lands early is reverted, or the maintainer opens the API phase.

- Smoke clients call only the latest published APIs. A release cut updates `test/smoke/clients` to the APIs being published, then verifies the affected smoke lanes.
- A release is cut by hand: a PR merging `main` into `release`, with a merge commit. After the pre-cut merge of `release` into `main`, run `just version 0.22.0` on `main` for the first shared cut, or the next shared minor for a later API or wire cut. Commit the resulting versions and lockfiles before opening the cut PR. Run `just version` to check them.
- A breaking cut first writes, on `main`, each changed package's version-transition section in its `MIGRATE.md` (`## 0.3 -> 0.4`), the only `MIGRATE.md` edit `just check` allows, from the migration audit: the `!` subjects since the `release` merge base, each of those PRs' Impact section, and the API diff.
- Suggest a `release` backport for any serious bug fix (crash, security, data loss, broken interop), whether found on `main` or reported against a release. Land it on `main` first, then port it onto `release` as a separate PR that keeps the published API, so it ships as a patch. A backport quest or PR names "the next release", never a version number, since the `release` bump can land first. The Release gate's API diff (`just api-diff --deny`) fails a backport that changes one; on the cut PR, whose head is `main`, it only reports, unless `phase` is `additive`.
- A bench-only backport ports `main`'s benchmarks onto `release`, so a pairing against `release` (`just bench origin/release`) compares them. It carries only benchmarks and their harness: bench targets, bench-only dev-dependencies, and `bench/`. It changes no shipped code, and it shims locally any API that `release` lacks, keeping each case's ID the same as on `main`. A case that measures code `release` doesn't have is left out, and the PR lists it. Merging one makes release-plz propose patch bumps for the crates it touches, though no shipped code changed.
- After Rust or JS publication, **Release remeasure** checks the published pins, runs `just test wire-compat --remeasure` on `main` when they change, and updates one pin-update PR on `release/planned-breaks` for the merge queue. A later measurement replaces the pending PR head and requests landing again. Its body preserves each break’s failing cells after the run logs expire. Check that run and its PR before considering the release complete. Dispatch the workflow with its default dry-run input to verify current pins without writing a PR.
- Nothing merges `release` into `main` automatically. Before every cut, the maintainer merges `release` into `main` once, so trunk carries the published versions, CHANGELOGs, and any `release`-only commits. It lands as a merge commit with `--admin`, never squashed, so the cut starts from an advanced merge base.

A cut's kind follows what it carries since the last cut:

- An **additive** cut carries no break. It publishes patches: `just version` is not run, release-plz proposes Rust patches on `release`, and each changed JS package and binding wrapper takes a patch bump on `main` before the cut PR (Go derives its patch in CI; FFI packages follow their Rust crate). It runs the readiness checks: the release-to-main merge, the API diff (no break), `just test wire-compat`, package dry-runs, draft checks, all-language interop, and the smoke clients.
- An **API** or **wire** cut advances the shared minor with `just version`, so a wire cut after an API cut advances it again. Besides the readiness checks, it needs the migration audit (its `MIGRATE.md` sections) and the API shape, feature, and Cargo feature reviews of what changed since the last breaking cut. A wire cut also finalizes the cycle's wire version.

# Versions

Releases are cut separately; bump only when asked. Every published MoQ package
shares one minor across ecosystems. The first shared cut is `0.22.0`; every
later API or wire cut advances the minor together, including unchanged
packages, and an additive cut publishes patches under the current minor. Patch
versions stay independent per package, including Rust: release-plz has no
`version_group`.

`kio`, `kio-macros`, `qmux`, and npm `@moq/qmux` keep their own version lines;
`@moq/signals` joins the shared minor. New experiments start at `0.0.x`.
A published library's normal or build dependency on an experimental package
in the same ecosystem promotes that package into the shared minor, including
optional and target-specific dependencies. Dev dependencies and binary-only
Rust dependents do not promote it. Unpublished/private packages are excluded.
Thus `moq-uring` joins through the relay library, while `moq-archive` stays
experimental through the binary-only CLI and a dev dependency.

`just version 0.22.0` sets eligible versions and workspace lock entries without
resolving unrelated dependencies. It also advances Python, Dart, and Kotlin
bindings requirements to the cut's minor while allowing independent patches.
Rust requirements are updated by
`release-plz set-version`; disposable changelogs keep published history intact.
The recipe records the shared minor and named exemptions in `version.json`,
which also holds the cycle's `phase` (see [Releases](#releases)); the check
accepts `null`, `additive`, `api`, or `wire`, and `null` disables the phase's
API-diff enforcement.
`just version` checks it on every `just check` and `just ci`; enforcement is
disabled while its minor is `null`, until the cut runs the recipe. Once active,
the check rejects minor drift, unpromoted dependencies, stale exemptions, and
mismatched npm/Python lock entries, and stale bindings requirements.

The nonpublishing fixture in `sh/version.test.py` verifies that release-plz's
`update` engine, also used by `release-pr`, preserves a manually set unpublished
minor despite breaking commits. `just gh test` runs the whole version suite on
every check, so a release-plz upgrade that breaks the cut fails before it lands.
No publish, tag, or release workflow runs as part of `just version`.

Each package's version lives in one place:

- **Rust**: release-plz owns crate versions and Rust dependency requirements.
- **JavaScript**: `js/*/package.json` packages with a `scripts.release` entry (skip private ones), plus the matching workspace version in `bun.lock`.
- **Python**: `py/moq-rs/pyproject.toml`, plus the matching `moq-rs` entry in the root `uv.lock`; `py/moq-ffi` follows the `moq-ffi-v*` tag and Rust crate.
- **Swift**: `swift/VERSION`. **Kotlin**: `moq.version` in `kt/gradle.properties`. **Dart**: `version` in `dart/moq/pubspec.yaml`. Their FFI counterparts track the Rust crate.
- **Go**: `go/wrapper/VERSION` holds a human-owned `MAJOR.MINOR` line; CI derives the independent patch; the shared cut sets the minor. Leave the placeholder FFI version in `go.mod` alone.
- **C**: `c/moq/VERSION`, human-owned; a `c-v<version>` tag releases it.
- **C++**: `cpp/moq/VERSION`, human-owned; a `cpp-v<version>` tag releases it.
- **OBS**: `cpp/obs/VERSION`, human-owned; each C++ release also cuts `obs-moq-v<version>`, and refuses one whose version already names an earlier release.
