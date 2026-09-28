# [S] Go and Dart doc samples compile against their wrappers

## Goal

The Go and Dart pages in `doc/lib/go/` and `doc/lib/dart/` are checked the
way [#4049](https://github.com/moq-dev/moq/pull/4049) checks Python, Kotlin,
Swift, and C: every fenced sample is extracted by `doc/lib/samples.sh` and
type-checked against the wrapper it documents (`go vet` for `go/wrapper`,
`dart analyze` for `dart/moq`), so a renamed wrapper method breaks the
language's check instead of silently breaking the docs. Today `samples.sh`
knows neither language.

## Plan

- Add `go` and `dart` to `samples.sh` with the same shape: one function per
  sample, imports hoisted, inputs a sample leaves undefined (`client`,
  `opusInit`, `pts`) supplied by a prelude the caller compiles alongside.
- Go is stricter than the others: a file needs a `package` clause, imports
  come as single lines or a parenthesized block, and unused locals and
  imports are compile errors. Handle what the samples actually use; prefer
  adjusting a sample so it reads naturally and still compiles over growing
  the extractor.
- Wire each into its language's check script, `sh/go/check.sh` and
  `sh/dart/check.sh` on the tooling line, the way `sh/kt/check.sh` and
  `sh/py/samples.sh` call `samples.sh`. Add `doc/lib/go/`, `doc/lib/dart/`,
  and `doc/lib/samples.sh` to the `go` and `dart` patterns of the impact
  map in `sh/dispatch.sh`, as the `py`, `kt`, and `swift` ones already have.
- Prove it by renaming one wrapper method locally and watching each check
  fail on the doc sample.

Public API: none. Wire: none.

## Required

- [Tooling](/quest/m1/tooling/README.md) - the `sh/<module>/` check scripts and the impact map this extends
