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
//
// # Cancellation
//
// Blocking calls take a context.Context. Most can be aborted: cancelling the
// context returns ctx.Err() and tears down the in-flight native task. A few
// calls have no native cancel, namely the producer-side Used/Unused waits and
// Server.Accept. For those, cancelling the context still returns ctx.Err()
// promptly, but the underlying wait keeps running on a background goroutine
// until it completes on its own (a subscriber arrives, the track is dropped, or
// the server is closed). So context cancellation is not a way to bound resource
// use on those calls; close the owning Server, or finish/drop the producer, to
// release them. Each such method documents this on itself.
package moq
