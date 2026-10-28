# [M] Go per-platform lib modules

## Goal

Each platform's staticlib ships in its own module nested under
`moq.dev/moq-ffi`. A `go build` from a tidy `go.sum` downloads only the
host's lib, about 16 MiB instead of 61 MiB, and `go mod vendor` builds.

## Plan

Prototyped against the published v0.4.16 libs on linux/amd64, served from a
static GOPROXY layout:

- Decided 2026-10-17: lib modules are `moq.dev/moq-ffi/lib/<goos>_<goarch>`,
  so the one `moq.dev/moq-ffi` vanity entry covers them. Each holds its
  staticlib plus a cgo file whose `LDFLAGS` name the archive and the system
  libraries that `moq/cgo.go` lists today, so the archive precedes its
  dependencies on the link line.
- The `moq` package imports the matching lib module from a
  `lib_<goos>_<goarch>.go` file. Its `go.mod` requires all four at its own
  version, in lockstep.
- Cross-package linking relies on the go command tolerating a failed cgo
  dynimport link (Go 1.19+, golang/go#52863). The prototype binary called into
  Rust.
- `go get`, `go mod tidy`, and `go mod download` still fetch all four, since
  module commands ignore build tags. Only `go build` and `go test` are lazy.
- `go mod vendor` fails today with `cannot find -lmoq_ffi`, because it skips
  the `.go`-less `lib/` dirs. The prototype vendored and built.
- Link-test all four targets in CI. darwin and windows are unverified.
  Windows keeps `-L${SRCDIR} -lmoq_ffi` rather than naming `moq_ffi.lib`.
- `sh/go/stage.sh` stages the host's lib module and `replace`s it too.

## Required

- [Go module proxy](/quest/m2/go-module-proxy.md) - only our proxy can add modules under `moq.dev/moq-ffi` without another git mirror
