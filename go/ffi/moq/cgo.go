// Package moq is the raw UniFFI binding layer for Media over QUIC, published as
// github.com/moq-dev/moq-go-ffi. Most callers want the ergonomic
// github.com/moq-dev/moq-go wrapper, which builds on this package.
//
// The exported API is generated from rs/moq-ffi via uniffi-bindgen-go and
// dropped in alongside this file (moq.go) by scripts/package-ffi.sh; the in-tree
// source therefore does not build on its own. Run scripts/check.sh to stage a
// complete copy into dist/ and exercise it.
//
// The per-platform static archive is loaded from moq/lib/<goos>_<goarch>/
// inside the staged module, populated by scripts/package-ffi.sh from the release
// build matrix.
package moq

// A Rust staticlib does not carry the link options its dependencies declare, so
// every system library has to be named here. The same list, for the same reason,
// lives in rs/libmoq/native-libs/ for C consumers of libmoq; keep the two in step
// when moq-ffi gains a dependency. The moq-video / moq-audio entries cover
// hardware H.264/H.265 encode and decode plus capture, and openh264 (the software
// H.264 fallback) is C++.
//
// ScreenCaptureKit sets a macOS 12.3 floor for darwin builds: it does not exist
// before then, so a binary linking it fails to load on 12.0-12.2 even when it
// never captures a screen. The Swift packages declare the same floor.

/*
#cgo linux,amd64 LDFLAGS: -L${SRCDIR}/lib/linux_amd64 -lmoq_ffi -ldl -lm -lpthread -lstdc++
#cgo linux,arm64 LDFLAGS: -L${SRCDIR}/lib/linux_arm64 -lmoq_ffi -ldl -lm -lpthread -lstdc++
#cgo darwin,amd64 LDFLAGS: -L${SRCDIR}/lib/darwin_amd64 -lmoq_ffi -framework Security -framework SystemConfiguration -framework CoreFoundation -framework CoreServices -framework Foundation -framework AVFoundation -framework CoreMedia -framework CoreVideo -framework VideoToolbox -framework ScreenCaptureKit -framework CoreGraphics -lc++
#cgo darwin,arm64 LDFLAGS: -L${SRCDIR}/lib/darwin_arm64 -lmoq_ffi -framework Security -framework SystemConfiguration -framework CoreFoundation -framework CoreServices -framework Foundation -framework AVFoundation -framework CoreMedia -framework CoreVideo -framework VideoToolbox -framework ScreenCaptureKit -framework CoreGraphics -lc++
#cgo windows,amd64 LDFLAGS: -L${SRCDIR}/lib/windows_amd64 -lmoq_ffi -lws2_32 -luserenv -lbcrypt -lntdll
*/
import "C"
