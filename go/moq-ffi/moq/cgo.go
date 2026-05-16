package moq

// Link against the moq-ffi staticlib produced by `cargo build --release -p moq-ffi`
// in the workspace target directory. The path is relative to this file's
// directory (${SRCDIR}) which is /go/moq-ffi/moq; the workspace target is
// three levels up at /target/release.
//
// To override (e.g. when the cargo target dir is elsewhere, or when consuming
// this module from outside the monorepo), set CGO_LDFLAGS at build time:
//
//   CGO_LDFLAGS="-L/path/to/target/release -lmoq_ffi" go build ./...
//
// CGO_CFLAGS must point at this directory so `#include <moq.h>` resolves:
//
//   CGO_CFLAGS="-I/path/to/go/moq-ffi/moq" go build ./...

// We pass the staticlib by full path (rather than -L ... -lmoq_ffi) to force
// static linking. With both libmoq_ffi.a and libmoq_ffi.so present in the
// release dir, ld would otherwise prefer .so and the resulting Go binary
// would need LD_LIBRARY_PATH at runtime.

// #cgo CFLAGS: -I${SRCDIR}
// #cgo linux,amd64 LDFLAGS: ${SRCDIR}/../../../target/release/libmoq_ffi.a -lm -ldl -lpthread
// #cgo linux,arm64 LDFLAGS: ${SRCDIR}/../../../target/release/libmoq_ffi.a -lm -ldl -lpthread
// #cgo darwin,amd64 LDFLAGS: ${SRCDIR}/../../../target/release/libmoq_ffi.a -framework Security -framework CoreFoundation -framework SystemConfiguration
// #cgo darwin,arm64 LDFLAGS: ${SRCDIR}/../../../target/release/libmoq_ffi.a -framework Security -framework CoreFoundation -framework SystemConfiguration
// #cgo windows,amd64 LDFLAGS: ${SRCDIR}/../../../target/release/moq_ffi.lib -lws2_32 -luserenv -lntdll -lbcrypt -lncrypt
import "C"
