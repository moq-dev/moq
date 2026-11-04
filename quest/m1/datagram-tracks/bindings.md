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

Until this lands, moq-ffi and moq-c back the old API with the new types
([moq-dev/moq#7752](https://github.com/moq-dev/moq/pull/7752)): every raw track
publishes a datagram track of the same name, and a reader subscribes to the
stream track before it opens the datagram track, so a datagram reader still
shows as the track's demand and reaches a dynamic publisher's request handler.
Remove that coupling here, and give the separate types their own demand (the
datagram producer's aggregate priority) and on-demand requests
(`Requests::datagrams()`), so a native datagram-only reader is visible too.

Public API: breaking for every binding's datagrams. Wire: none.
