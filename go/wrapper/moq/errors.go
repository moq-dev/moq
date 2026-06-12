package moq

import (
	"context"
	"errors"

	ffi "github.com/moq-dev/moq-go-ffi/moq"
)

// Error is the error type returned across the FFI boundary. Compare against the
// sentinels below with errors.Is, or use IsShutdown for the common case.
type Error = ffi.MoqError

// Configuration errors returned by the wrapper itself (not the FFI layer).
var (
	ErrNoPublishOrigin = errors.New("moq: no publish origin configured")
	ErrNoConsumeOrigin = errors.New("moq: no consume origin configured")
)

// IsShutdown reports whether err is the expected result of a graceful shutdown
// (Cancelled or Closed) rather than an actual failure. It's the value to check
// when a stream ends because its consumer was cancelled or the session closed.
func IsShutdown(err error) bool {
	return errors.Is(err, ffi.ErrMoqErrorCancelled) || errors.Is(err, ffi.ErrMoqErrorClosed)
}

// runCancellable runs a blocking FFI call on a goroutine and races it against
// ctx. uniffi-bindgen-go renders Rust async fns as blocking Go calls with no
// context parameter, so cancellation is wired by calling the object's own
// cancel() (which aborts the in-flight task) when ctx is done. The blocked
// goroutine then unwinds on its own and is discarded.
func runCancellable[T any](ctx context.Context, cancel func(), call func() (T, error)) (T, error) {
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
		var zero T
		return zero, ctx.Err()
	case r := <-ch:
		return r.val, r.err
	}
}

// runErr is runCancellable for calls that return only an error.
func runErr(ctx context.Context, cancel func(), call func() error) error {
	_, err := runCancellable(ctx, cancel, func() (struct{}, error) {
		return struct{}{}, call()
	})
	return err
}
