# [XS] The Go docs point at moq.dev/moq again

## Goal

`doc/lib/go/index.md` names one import path, `moq.dev/moq`, in the badge, the
import sample, and the API reference link, and every link resolves: the
pkg.go.dev page exists and `go get moq.dev/moq@latest` succeeds.

## Plan

The vanity path works today. `https://moq.dev/moq?go-get=1` serves the
`go-import` meta tag, and the module proxy lists `moq.dev/moq` versions. What
is missing is a release: the rename to `module moq.dev/moq` (#2957) reached
`main` on 2026-09-20 through the dev merge (#3793), after the last
`moq-ffi-v0.3.19` tag (2026-09-17). Every published mirror tag still declares
`github.com/moq-dev/moq-go` and `github.com/moq-dev/moq-go-ffi`, so pkg.go.dev
rejects `moq.dev/moq` and `go get` fails on the path mismatch.
`release-go.yml` sees this and defers on every `main` push until the next ffi
release republishes both mirrors under the vanity paths.

#3847 papered over the gap by pointing the badge, `import`, and API reference
at `github.com/moq-dev/moq-go`, leaving the page half on each path, and the
`import` line does not match the old mirror layout either (the package sat at
`github.com/moq-dev/moq-go/moq`).

Revert #3847 (`git revert 62795f159`) and confirm both
`https://pkg.go.dev/moq.dev/moq` and `https://pkg.go.dev/moq.dev/moq-ffi`
return 200. If the chain did not re-cut the wrapper, look at the Release Go run
for the ffi tag before touching the docs.

## Related

- [Release](/quest/m0/release.md) - the release that cuts the tag
