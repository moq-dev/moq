// Package moq is the ergonomic Go API for Media over QUIC: real-time pub/sub
// with built-in caching, fan-out, and prioritization.
//
// It wraps the raw UniFFI bindings in github.com/moq-dev/moq-go-ffi with
// idiomatic Go: context.Context cancellation, Go error returns, and Go 1.23
// range-over-func iterators (iter.Seq2) for live streams. The raw record and
// enum types are re-exported here without the Moq prefix (see types.go), so
// most programs never need to import the ffi package directly.
//
// A typical full-duplex client wires a single origin as both publish source
// and consume sink; Dial does this automatically when no origin is supplied.
// See the package README for a runnable example.
package moq
