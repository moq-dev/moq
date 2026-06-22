package moq

import "testing"

// TestLink is a link smoke test. The moq package is a cgo library, so
// `go build ./...` compiles it without ever linking an executable. That lets a
// missing system library in the per-platform LDFLAGS (see cgo.go) pass the
// build and only fail when a downstream consumer links a binary, off-platform
// from CI. `go test` does link a real test binary against the static archive,
// and the package's generated init references the FFI symbols, so the archive
// and its transitive system deps (e.g. CoreServices, which the bundled
// FSEvents backend needs on macOS) are pulled in and resolved here instead.
func TestLink(t *testing.T) {}
