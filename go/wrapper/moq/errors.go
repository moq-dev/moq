package moq

import (
	"context"
	"errors"

	ffi "github.com/moq-dev/moq-go-ffi/moq"
)

// Error is the error type returned across the FFI boundary. Compare against the
// sentinels below with errors.Is, or use one of the Is* helpers for the common
// cases.
type Error = ffi.MoqError

// Configuration errors returned by the wrapper itself (not the FFI layer).
var (
	ErrNoPublishOrigin = errors.New("moq: no publish origin configured")
	ErrNoConsumeOrigin = errors.New("moq: no consume origin configured")
)

// Error sentinels re-exported from the ffi layer without the MoqError prefix, so
// callers can errors.Is against them without importing moq-go-ffi directly.
// These mirror the variants of the native error enum; transparent variants
// (Protocol, Media, ...) wrap a lower-level error whose detail survives in the
// message.
var (
	ErrProtocol         = ffi.ErrMoqErrorProtocol
	ErrMedia            = ffi.ErrMoqErrorMedia
	ErrMux              = ffi.ErrMoqErrorMux
	ErrAudio            = ffi.ErrMoqErrorAudio
	ErrURL              = ffi.ErrMoqErrorUrl
	ErrTimeOverflow     = ffi.ErrMoqErrorTimeOverflow
	ErrLogLevel         = ffi.ErrMoqErrorLogLevel
	ErrTask             = ffi.ErrMoqErrorTask
	ErrCancelled        = ffi.ErrMoqErrorCancelled
	ErrClosed           = ffi.ErrMoqErrorClosed
	ErrConnect          = ffi.ErrMoqErrorConnect
	ErrBind             = ffi.ErrMoqErrorBind
	ErrReject           = ffi.ErrMoqErrorReject
	ErrAlreadyResponded = ffi.ErrMoqErrorAlreadyResponded
	ErrCodec            = ffi.ErrMoqErrorCodec
	ErrInvalidErrorCode = ffi.ErrMoqErrorInvalidErrorCode
	ErrUnauthorized     = ffi.ErrMoqErrorUnauthorized
	ErrForbidden        = ffi.ErrMoqErrorForbidden
	ErrNotFound         = ffi.ErrMoqErrorNotFound
	ErrUnsupported      = ffi.ErrMoqErrorUnsupported
	ErrLog              = ffi.ErrMoqErrorLog
)

// IsShutdown reports whether err is the expected result of a graceful shutdown
// (Cancelled or Closed) rather than an actual failure. It's the value to check
// when a stream ends because its consumer was cancelled or the session closed.
func IsShutdown(err error) bool {
	return errors.Is(err, ErrCancelled) || errors.Is(err, ErrClosed)
}

// IsAuthError reports whether err is an authentication/authorization failure
// (the FFI Unauthorized or Forbidden variants, i.e. HTTP 401/403).
func IsAuthError(err error) bool {
	return errors.Is(err, ErrUnauthorized) || errors.Is(err, ErrForbidden)
}

// runCancellable runs a blocking FFI call on a goroutine and races it against
// ctx. uniffi-bindgen-go renders Rust async fns as blocking Go calls with no
// context parameter, so cancellation is wired by calling the object's own
// cancel() (which aborts the in-flight task) when ctx is done. The blocked
// goroutine then unwinds on its own and is discarded; the result channel is
// buffered so that send never blocks and the goroutine can't leak.
//
// When cancel is nil there is no way to abort the underlying call, so a
// cancelled ctx returns ctx.Err() immediately while the goroutine stays parked
// until the call completes on its own. See the package doc for the consequences.
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
