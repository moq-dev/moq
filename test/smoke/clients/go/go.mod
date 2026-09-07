// Not a published module. test/smoke/smoke.sh builds this out of a scratch copy
// and rewrites the require below into a `replace` pointing at the wrapper that
// go/scripts/stage.sh assembles from this checkout, so nothing resolves from the
// module proxy. The placeholder version keeps the committed file honest: there
// is no such tag, and there is no go.sum because every dependency is local.
module moq.dev/smoke

go 1.23

require github.com/moq-dev/moq-go v0.0.0
