// Not a published module. test/interop/interop.sh builds this out of a scratch copy
// and rewrites the require below into a `replace` pointing at the wrapper that
// sh/go/stage.sh assembles from this checkout, so nothing resolves from the
// module proxy. The placeholder version keeps the committed file honest: there
// is no such tag, and there is no go.sum because every dependency is local.
module moq.dev/interop

go 1.23

require moq.dev/moq v0.0.0
