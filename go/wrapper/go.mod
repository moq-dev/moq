module github.com/moq-dev/moq-go

go 1.23

// The require below is a placeholder. The real version is rewritten at release
// time by go/scripts/package-wrapper.sh to the latest published moq-go-ffi, and
// `just go check` injects a local `replace` to the freshly-generated bindings.
// Do not "fix" this by hand or add a replace directive to the committed file.
require github.com/moq-dev/moq-go-ffi v0.0.0
