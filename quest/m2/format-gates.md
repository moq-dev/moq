# [M] Every handwritten language has a format gate

## Goal

`just check` and `just fix` format and check Go, Kotlin, Swift, and the C/C++
wrappers the way they already do Rust, TypeScript, shell, and Python, and Ruff
covers every Python file in the repo. Generated bindings stay excluded.

## Plan

From the 2026-10-27 coverage audit. On main at 8d2fa6653a, Go runs
`go vet` but no formatter (`gofmt -l go/wrapper` lists `client.go`,
`errors_test.go`, `audio/audio.go`, and `video/video.go`), Kotlin and Swift
only compile, clang-format runs only for `cpp/obs` (`sh/obs/check.sh`), and
the root Ruff config does not reach `sh/`, `test/`, or `bench/` Python.

Decided 2026-10-27 (maintainer):

- Pure formatters, like rustfmt and gofmt: `gofmt`, `ktfmt` (kotlinlang
  style), Apple's `swift-format` (bundled with the Swift 6 toolchain as
  `swift format`), and `clang-format` reusing OBS's config for `c/` and
  `cpp/` wrappers. No style linters (ktlint, SwiftFormat rules) in this quest.
- Take each tool from the Nix dev shell so CI and local runs match.
- One commit per language: add the check and fix recipes, then the
  reformatting churn.
- Extend Ruff's lint to the uncovered Python first, then its formatter,
  each as its own commit.

Public API: none. Wire: none.
