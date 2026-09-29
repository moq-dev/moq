// Package bridge is what moq.dev/moq shares with its subpackages: the context
// bridge for blocking FFI calls, and the generated handles behind the root
// package's wrapper types.
package bridge

import (
	"context"
	"iter"
)

// Handle is a native object the wrapper owns. Every uniffi-generated object has
// Destroy, which drops the Rust-side Arc, and comparable so an optional one can
// be checked for nil before it is destroyed.
type Handle interface {
	comparable
	Destroy()
}

// run races a blocking FFI call against ctx.
//
// `cancel` is the object's own cancel() for a call that owns a stream, session,
// or listener, so a cancelled Next still ends the stream and a cancelled Connect
// still tears the client down. The generated binding is given a context that
// does not cancel with the caller (see objectCallContext); otherwise it can
// return ctx.Err() into the result channel first and skip that abort. The
// blocked goroutine then unwinds on its own; the result channel is buffered so
// its send never blocks and it can't leak.
//
// release, for a call that returns a native handle, disposes of a result nobody
// received. Cancelling is not a retraction: select picks at random when the
// deadline and the result are both ready, so a call can succeed and still lose,
// and the handle it produced is live either way.
func run[T any](ctx context.Context, cancel func(), call func() (T, error), release func(T)) (T, error) {
	// A context that is already done starts no native work at all. Racing it
	// instead would let a call that resolves immediately (a subscribe to a track
	// already there) win the select and hand back a live handle the caller asked
	// not to have, and every such call has side effects on the way.
	if err := ctx.Err(); err != nil {
		var zero T
		return zero, err
	}

	type result struct {
		val T
		err error
	}
	ch := make(chan result, 1)
	go func() {
		val, err := call()
		ch <- result{val, err}
	}()

	select {
	case <-ctx.Done():
		if cancel != nil {
			cancel()
		}
		if release != nil {
			// Exactly one receiver takes the result, so it is this one once the
			// caller has given up. Waiting here would put the native unwind on
			// the caller's deadline, so it happens off to the side.
			go func() {
				if r := <-ch; r.err == nil {
					release(r.val)
				}
			}()
		}
		var zero T
		return zero, ctx.Err()
	case r := <-ch:
		return r.val, r.err
	}
}

// Call runs a blocking FFI call that yields no handle of its own, so a
// result the caller never sees costs nothing to drop.
//
// cancel is the object's own cancel() for a call that owns its stream, session,
// or listener. One-shot calls pass ctx straight to the generated binding.
func Call[T any](ctx context.Context, cancel func(), call func(context.Context) (T, error)) (T, error) {
	return run(ctx, cancel, func() (T, error) { return call(objectCallContext(ctx, cancel)) }, nil)
}

// CallHandle is Call for a call that returns a native handle, which is
// destroyed rather than abandoned when the caller has already given up. Left to
// the Go finalizer it would stay live in the meantime: a subscription still
// running on the wire, or an incoming request accepted and never answered.
func CallHandle[T Handle](ctx context.Context, cancel func(), call func(context.Context) (T, error)) (T, error) {
	return run(ctx, cancel, func() (T, error) { return call(objectCallContext(ctx, cancel)) }, releaseHandle[T])
}

// objectCallContext is the context the generated binding sees. Object-owned
// calls already abort via cancel; sharing the caller's ctx lets the binding
// return ctx.Err() first and skip that abort, leaving the stream, session, or
// request active.
func objectCallContext(ctx context.Context, cancel func()) context.Context {
	if cancel == nil {
		return ctx
	}
	return context.WithoutCancel(ctx)
}

// CallErr is Call for calls that return only an error.
func CallErr(ctx context.Context, cancel func(), call func(context.Context) error) error {
	_, err := Call(ctx, cancel, func(ctx context.Context) (struct{}, error) {
		return struct{}{}, call(ctx)
	})
	return err
}

// releaseHandle drops a handle the caller never received. A successful call can
// still yield none (an Accept once the server has stopped), which is the zero
// value rather than something to destroy.
func releaseHandle[T Handle](val T) {
	var zero T
	if val != zero {
		val.Destroy()
	}
}

// Seq turns an end-on-nil Next method (returning a pointer or interface)
// into a Go 1.23 range-over-func sequence. It yields (value, nil) for each item,
// yields (nil, err) once if a call fails, and stops cleanly when Next returns nil
// (the stream ended) or when the consumer breaks out of the range loop.
//
//	for frame, err := range consumer.Frames(ctx) {
//	    if err != nil {
//	        if moq.IsShutdown(err) { break }
//	        return err
//	    }
//	    // use frame
//	}
func Seq[T comparable](ctx context.Context, next func(context.Context) (T, error)) iter.Seq2[T, error] {
	return func(yield func(T, error) bool) {
		var zero T
		for {
			val, err := next(ctx)
			if err != nil {
				yield(zero, err)
				return
			}
			if val == zero {
				return
			}
			if !yield(val, nil) {
				return
			}
		}
	}
}
