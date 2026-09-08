# Packaged consumer QA

Build the release candidates this checkout would publish, then consume them from
outside the workspace.

```bash
just test packaged                 # lanes selected from the branch diff
just test packaged --rust --js     # both lanes, whatever the diff says
just test packaged --self-test     # the negative controls
just test packaged --keep          # leave the staging directory behind
```

## Why

Every existing gate looks at the packages from the inside. Cargo builds them as
workspace members whose siblings are path dependencies. Bun links them into one
hoisted `node_modules`. Both supply what an archive omits, so all of these pass
in the workspace and break for the first consumer:

- a source file the `.crate` archive does not ship
- a package a JS entry point imports but does not declare
- a path dependency with no `version`, which cannot be published at all
- a `dist/` that `bun run build` produced but `files`/`exports` do not expose

`moq-dev/smoke` catches these, but only after a release, from a public registry.
This is the bridge: the archives a release would upload, consumed by a directory
that has never heard of this repo.

Versions are never bumped and nothing is published. This is verification only.

## Lanes

Both lanes stage the changed packages plus the unpublished siblings they need,
in dependency order, and both stage into a `mktemp -d` outside the checkout.
That location is not cosmetic: cargo, npm, and bun all walk up from the current
directory looking for a workspace root or a config file, so a consumer staged
anywhere under the repo would quietly rejoin it.

### Rust

1. `audit.sh` refuses a publishable crate whose path dependency carries no
   version, naming the crate and the dependency. Cargo refuses too, but from
   inside a `Packaging` step whose message reads as a cargo internal.
2. `cargo package --locked --no-verify --allow-dirty` per candidate, which is
   the same archive `release-plz` uploads, built against the committed
   `Cargo.lock`.
   `--no-verify` because cargo's own verification build resolves siblings from
   crates.io, where an unreleased version does not exist. Step 4 replaces it and
   is stronger: it patches the staged siblings in, so it reports what the
   archive contains rather than what the registry happens to hold.
3. Each archive is extracted into the staging directory. The consumer only ever
   sees the extraction, never the workspace's `target/`, so a file missing from
   the archive is a compile error instead of a silent hit on the build the
   workspace already has.
4. A generated consumer crate depends on every candidate and carries a
   `[patch.crates-io]` entry pointing at its extraction. That is the explicit
   candidate-resolution step: no name we staged can float to the registry, and a
   name we failed to stage fails at resolution rather than testing a release
   that already shipped.

A binary-only crate (`moq-cli`, `moq-boy`) cannot be depended on, so it is
consumed the other way round: a copy of its extraction is built in a throwaway
workspace that owns the patch table, leaving the archive as cargo produced it.

The consumer body is a released-API fixture per crate that has one under
`rust/api/`, and a bare `use <crate> as _;` for the rest. The bare link still
compiles the whole archive, which is what catches a missing file. The fixtures
add what `cargo-semver-checks` cannot see: whether code a consumer already wrote
still compiles. A fixture that stops compiling is a break to classify against
the repo's `main`/`dev` targeting rule, not a fixture to edit.

Default features only, matching `just check`. Pass `--features` for the
combination a change actually touches, e.g.
`just test packaged --rust --features moq-native/quiche`.

### JavaScript

1. `bun run build` per candidate, which rewrites `package.json` for publication,
   resolves `workspace:` ranges to `^<current version>`, and runs publint.
2. `npm pack` from `dist/`, so the tarball holds the published manifest rather
   than the source one.
3. `npm ci` in the consumer against a committed lockfile, which freezes the
   consumer's own tooling, followed by an explicit `npm install` of the
   candidate tarballs. Every candidate is both a direct dependency and an
   `overrides` entry, so a transitive `@moq/x: ^1.2.3` resolves to the staged
   tarball instead of whatever the registry currently serves.
4. The install is then asserted, not assumed: every candidate must resolve from
   a `file:` specifier, and `node_modules/@moq` must contain no symlinks. A
   candidate served from the registry is the one outcome that looks like a pass
   and is not one.
5. Every documented entry point (the `exports` keys of the published manifest)
   is imported under node, then bundled for the browser with `bun build`. A
   package that declares side-effectful entry points is a web-component package
   whose entry points need a DOM, so those go to the bundle only. The bundle is
   where the assets get checked: the audio worklets are inlined as blob URLs by
   each package's own vite build, and the libav WASM arrives as a normal
   dependency.
6. `@moq/net` is the layer everything else rides on, so when it is a candidate
   the consumer also connects to a relay built from this checkout, publishes a
   frame through it, and reads that frame back. That one runs under bun, because
   `@moq/web-transport` ships TypeScript sources node refuses to strip inside
   `node_modules`. `PACKAGED_ROUNDTRIP=0` skips it.

The committed `js/consumer/package-lock.json` is the one npm lockfile in a bun
repo, and `.gitignore` has a negation for it. It exists because the consumer
must install without a workspace: npm is what resolves `overrides` against local
tarballs and records where each package came from, which is what step 4 reads.

## Negative controls

`--self-test` breaks a candidate on purpose, three ways, and requires the
matching check to fail:

| Fixture                                            | Must fail            |
| -------------------------------------------------- | -------------------- |
| a path dependency with its `version` removed        | `audit.sh`           |
| a module file removed from the extracted archive    | the consumer build   |
| a dependency removed from a packed `dist/package.json` | the entry-point import |

Each is built from a copy inside the staging directory, so nothing touches the
checkout. The archive fixture also asserts the intact archive builds first: a
check that fails on everything is not a check.

## Reading the report

Each lane prints one line per archive: name, version, SHA-256 digest, and
whether the archive was requested or dragged in as a sibling. Only a requested
archive is reported as verified; a sibling is staged so the build can happen at
all, and is only as exercised as that build made it. The consumer manifest path
and the exact command are printed too, so a failure can be reproduced by hand
with `--keep`.
