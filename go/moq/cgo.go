// Package moq provides Go bindings for Media over QUIC.
//
// The exported API is generated from rs/moq-ffi via uniffi-bindgen-go and
// dropped in alongside this file (moq.go) by scripts/package.sh; the in-tree
// source therefore does not build on its own. Run scripts/check.sh to stage a
// complete copy into dist/ and exercise it.
//
// The per-platform static archive is loaded from moq/lib/<goos>_<goarch>/
// inside the staged module, populated by scripts/package.sh from the release
// build matrix.
package moq

/*
#cgo linux,amd64 LDFLAGS: -L${SRCDIR}/lib/linux_amd64 -lmoq_ffi -ldl -lm -lpthread
#cgo linux,arm64 LDFLAGS: -L${SRCDIR}/lib/linux_arm64 -lmoq_ffi -ldl -lm -lpthread
#cgo darwin,amd64 LDFLAGS: -L${SRCDIR}/lib/darwin_amd64 -lmoq_ffi -framework Security -framework SystemConfiguration -framework CoreFoundation -framework CoreServices
#cgo darwin,arm64 LDFLAGS: -L${SRCDIR}/lib/darwin_arm64 -lmoq_ffi -framework Security -framework SystemConfiguration -framework CoreFoundation -framework CoreServices
#cgo windows,amd64 LDFLAGS: -L${SRCDIR}/lib/windows_amd64 -lmoq_ffi -lws2_32 -luserenv -lbcrypt -lntdll
*/
import "C"
