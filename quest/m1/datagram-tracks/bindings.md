# [M] Bindings datagram tracks

## Goal

moq-ffi, moq-c, and every wrapper (Python, Swift, Kotlin, Go, Dart, C++)
expose the separate datagram producer and consumer types instead of
datagram methods on stream tracks, mirroring Rust's names.

## Plan

Follow the [epic](/quest/m1/datagram-tracks/README.md)'s decisions. Regenerate the
UniFFI outputs, update `go/wrapper` (`publish.go`, `subscribe.go`,
`records.go`) and `dart/moq`, and record the migration in each package's
`MIGRATE.md`. Update `doc/lib/*` where they show datagrams.

Public API: breaking for every binding's datagrams. Wire: none.

## Required

- [Rust datagram tracks](/quest/m1/datagram-tracks/rust.md) - the Rust types the bindings mirror
