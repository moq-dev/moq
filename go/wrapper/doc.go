// Package moq is the ergonomic Go API for Media over QUIC: real-time pub/sub
// with built-in caching, fan-out, and prioritization.
//
// It wraps the raw UniFFI bindings in moq.dev/moq-ffi with
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
// Every call that can block takes a context.Context, and cancelling it (or
// hitting its deadline) returns ctx.Err() promptly and tears the in-flight
// native work down with it. Nothing is left running in the background, so a
// per-call deadline is a real bound on resource use.
//
// What a cancel tears down depends on the call. A one-shot call (a subscribe, a
// fetch, RequestBroadcast, Resolve, a producer's Used/Unused, Server.Accept)
// aborts on its own and leaves the object it was made on usable, so the same
// broadcast, producer, or server takes the next call. A stream read (any Next,
// RecvGroup, ReadFrame, or the iterators over them) instead cancels the stream
// it reads, which is what a range loop over a cancelled context wants: the
// consumer is finished, not merely this read. Cancel a stream read only when you
// are done with the stream; to bound one read, cancel the whole consumer's
// context.
package moq
