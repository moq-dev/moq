# [M] The hand-written C API ships as libmoq, its final line

## Goal

The hand-written C API is published at the cut as `libmoq` 0.22, under the
package identity its users already have: the `libmoq` crate, the
`libmoq-v*` tag family and release archives, and the CMake and pkg-config
names `release` ships. A C user moves from 0.6 to 0.22 by following
`MIGRATE.md` for the API, with no package rename on top. 0.22 is that
line's last minor: afterwards it takes fixes only.

`moq-c` and `moq::c` name only the generated package (`c/moq`), which stays
unpublished until it is ready. The code-free `rs/libmoq` stub is gone, and
the docs no longer call either C API a stable ABI.

## Plan

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit): ✅ ship
the hand-written API as `libmoq` 0.22 and keep `moq-c` unpublished for the
generated API (rejected: publishing it as `moq-c` and replacing that
package's whole API one minor later; shipping no C at 0.22). This reverses
the 2026-10-25 decision that the hand-written crate ships as `moq-c`, and
the rename in [moq-dev/moq#4288](https://github.com/moq-dev/moq/pull/4288).
C users were facing three names and two rewrites; this leaves one rename,
at the same moment as the API change that needs a rewrite anyway.

Facts found by the audit:

- Two packages in the tree claim one identity. `rs/moq-c` (hand-written):
  `project(moq-c)` (`rs/moq-c/CMakeLists.txt:17`), `moq::c` (`:66`),
  `moq-c.pc`, crate `moq-c` 0.7.0. `c/moq` (generated): `project(moq-c)`
  (`c/moq/CMakeLists.txt:10`), `moq::c` (`:61`), `OUTPUT_NAME moq` (`:64`).
- `moq-c` has never been published in either form: there are 79
  `libmoq-v*` tags, newest `libmoq-v0.6.25`, and no `moq-c-v*` or `c-v*`
  tag. `release` ships crate `libmoq` 0.6.26 with `project(moq)`, an
  interface target `moq`, `moq.pc`, and `.github/workflows/libmoq.yml`.
- The generated line cannot ship at this cut: its type names and callback
  contract change with [the exporter switch](/quest/m3/ffi-exporter.md).
- The plan this replaces published the hand-written crate as `moq-c` for
  one minor and then gave the name to a total rewrite (CamelCase types,
  `MoqError *` returns, task handles in place of `u32` ids), and no quest
  planned the pointer release for the crate that would die.
- `doc/lib/index.md:43` and `doc/lib/c/index.md:3,11` call the hand-written
  API a "stable ABI", though it is deleted once the generated one ships.
- `rs/libmoq` on `main` is a code-free stub at 0.6.12 whose description
  reads "Renamed to moq-c".

For the PR:

- Replace the stub: `rs/moq-c` becomes `rs/libmoq`, crate `libmoq`, at a
  version the cut's `just version` raises to the shared minor. Restore the
  CMake project, target, and pkg-config names `release` has
  (`git show origin/release:rs/libmoq/CMakeLists.txt`), and name the
  workflow and tag family `libmoq` again. Diff the installed layout
  (`include/`, `lib/`, `lib/cmake`, `lib/pkgconfig`) against a
  `libmoq-v0.6.25` archive and explain every difference in the PR.
- Follow every reader: `.github/workflows/moq-c.yml`, `nix/overlay.nix` and
  `flake.nix` (the `moq-c` package), `rs/justfile` (`package-moq-c`),
  `sh/api-diff/ffi.sh` (it already accepts both tag families),
  `sh/version.py`, `test/interop/clients/c`, `test/smoke/clients/c`,
  `doc/lib/c`, and `RELEASE.md`'s `moq-c-v*` release family.
- `rs/moq-c/native-libs/` and `cpp/moq/cmake/moq-ffi.cmake` are read by the
  generated package and C++ too; keep one source and repoint the readers
  (`rg 'moq-c/native-libs|moq-ffi\.cmake'`).
- `MIGRATE.md`: the hand-written file's "Unreleased" section becomes
  `libmoq` 0.6 to 0.22 and loses the package-rename steps. `c/moq/MIGRATE.md`
  keeps the move to the generated API for when that publishes.
- The Nightly size report names four `moq-c-<target>` archives, and
  `sh/size/report.py` refuses a renamed series. Rename them in the report
  and restart the history with the delete command in `sh/size/README.md`, or
  land with it.
- Docs: strike "stable ABI" from `doc/lib/index.md` and `doc/lib/c`, and say
  what is true: `libmoq` 0.22 is the hand-written API's last minor, and a
  generated `moq-c` replaces it.
- Recommended: say in `rs/libmoq/README.md` that the crate takes fixes only
  after the cut, so new moq-ffi features stop being mirrored into its 261
  hand-written exports. That is AGENTS.md's bindings rule
  ("a feature reaches the bindings when a binding user needs it") applied
  to C.
- Verify with `just rs check` for the crate, its `c-tests`, the C doc
  samples, and `just test interop --all`.

Public API: none beyond what `main` already changed; the package identity
returns to `libmoq`, so the published name does not change. Wire: none.

## Related

- [Retire the hand-written C crate](/quest/m3/release/c-retire.md) - deletes this crate once the generated `moq-c` publishes
- [Generated C bindings](/quest/m3/c/README.md) - the package that takes the `moq-c` name
