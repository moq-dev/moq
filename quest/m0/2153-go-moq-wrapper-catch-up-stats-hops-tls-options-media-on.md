# [S] go/moq: wrapper catch-up (stats, hops, TLS options, media-on-track)

## Goal

Implement and verify the behavior tracked in [#2153](https://github.com/moq-dev/moq/issues/2153)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

The hand-written Go wrapper (`go/wrapper/moq`) lags the generated moq-ffi surface more than the other wrappers. Missing today:

- `Session.Stats()` (connection stats snapshot; py/swift/kt have it)
- `Announcement.Hops()`
- Client TLS options: roots, system roots, certificate fingerprints (only `WithTLSDisableVerify`/`WithBind` exist in `go/wrapper/moq/client.go`)
- `PublishMediaOnTrack` and `MoqVideoHint` support
- Datagrams / subscription update / track info once those land in moq-ffi (see their issues)

Also, from the #2142 review: `FetchGroup` should take a `ctx` like `RecvGroup`/`RequestedGroup` (via `runCancellable`) so a fetch that misses the cache can be cancelled; that may need a cancel handle on the ffi fetch object.

All additive. Update `doc/lib/go` alongside per the Cross-Package Sync table.

## Closes

- [#2153](https://github.com/moq-dev/moq/issues/2153) - close this issue when the quest finishes
