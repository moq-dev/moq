# [M] The interop matrix has no Go client

## Goal

`test/smoke` publishes from and subscribes with Go, so a regression in
`go/wrapper` fails CI instead of shipping.

Boundaries: the Go wrapper only, through the same harness the other bindings
use. No new transport or feature coverage; this closes a hole in the matrix
rather than widening it.

## Plan

`just test smoke-full` runs 21 cells today: rust, python and js publishers
against rust, python, js, js-native-node, js-native-bun, c and gst
subscribers. Go appears in neither axis, so nothing in CI exercises the Go
wrapper end to end. Its only coverage is `go/wrapper`'s own tests, which run
against the library rather than across a session.

That gap is load-bearing: `moq-ffi` changes are required to run `smoke-full`
(Cross-Package Sync), and a passing run currently proves nothing about Go even
though `go/wrapper` is generated from the same core.

Add a Go publisher and a Go subscriber to `test/smoke/smoke.sh` alongside the
existing clients. `smoke.sh` resolves dependencies from committed lock data,
so the Go client must build with the repository's pinned toolchain and module
graph rather than resolving at run time.

### How it was found

Adding `context.Context` cancellation to every blocking Go operation. The PR
ran `smoke-full` and it passed 21/21, which said nothing at all about the Go
code the PR changed.

## Related

- [Native Go context](/quest/m1/go-native-context.md) - the Go wrapper work whose verification this gap limits
