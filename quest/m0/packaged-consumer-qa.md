# [M] Test Rust and JS packages from outside the workspace

## Goal

Changed Rust and JS packages install and work in an isolated consumer before publication.
Workspace linking, path dependencies, generated files, or a shared node_modules
directory cannot hide missing release contents or undeclared dependencies.

## Plan

The repository already builds JS packages, tests release scripts, and runs
source interop. `moq-dev/smoke` tests published releases. Add the missing bridge:
consume candidate archives from this checkout without publishing them.

- Cover Rust and JS, using their existing packaging commands and committed
  lock data. Stage the changed packages and required unpublished siblings in
  dependency order. Inspect archive manifests and build isolated consumers
  against those archives; do not copy workspace build outputs into the consumer.
- For Rust, exercise Cargo's packaged manifest and archive, including feature
  combinations relevant to the change. A local path dependency must retain the
  version metadata required to publish. Distinguish unreleased sibling blockers
  from an archive that actually passed a consumer build.
- For JS, install packed archives without workspace symlinks, import documented
  entry points in Node/Bun as applicable, and bundle a browser consumer including
  required worklet/WASM assets. Use frozen consumer lockfiles and an explicit
  candidate-resolution step rather than floating registry dependencies.
- Compile a small released-API consumer unchanged against the candidate to
  supplement semver tooling. A signature change or missing export must be
  classified against CONTRIBUTING's main/dev and 0.0.x rules.
- Run a minimal connection/media round trip for the representative consumer,
  and select the lane for manifest, exports, build-script, and package-source
  changes. Do not bump versions or publish as part of this verification recipe.

Acceptance: remove a required archive file, omit an imported JS dependency, and
remove a Rust dependency's publishable version in disposable fixtures. Each
fails the appropriate consumer check while a complete candidate passes without
access to the workspace's generated outputs. Record archive digests and exact
consumer commands for review.

## Related

- [Declared JS dependencies](/quest/m0/3361-js-every-moq-package-a-package-imports-is-declared.md) - owns the existing dependency declaration defect
- [Dart on iOS](/quest/m2/dart-ios.md) - owns native-asset loading on Apple devices
- [Ship capture and playback](/quest/m2/cli-packaging.md) - owns release feature enablement
