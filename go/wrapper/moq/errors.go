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
	// ErrNoPublishOrigin is returned when a publish operation is attempted but the server has no publish origin configured.
	ErrNoPublishOrigin = errors.New("moq: no publish origin configured")
)

// Error sentinels re-exported from the ffi layer without the MoqError prefix, so
// callers can errors.Is against them without importing moq-go-ffi directly.
// These mirror the variants of the native error enum; transparent variants
// (Protocol, Media, ...) wrap a lower-level error whose detail survives in the
// message.
var (
	// ErrProtocol matches a lower-level moq-net transport or protocol error; the underlying detail survives in the message.
	ErrProtocol = ffi.ErrMoqErrorProtocol
	// ErrMedia matches a media error from the hang layer, such as a malformed catalog or container.
	ErrMedia = ffi.ErrMoqErrorMedia
	// ErrMux matches a muxing or demuxing failure from moq-mux.
	ErrMux = ffi.ErrMoqErrorMux
	// ErrAudio matches a raw-audio encode or decode failure.
	ErrAudio = ffi.ErrMoqErrorAudio
	// ErrURL matches a malformed URL passed when connecting or publishing.
	ErrURL = ffi.ErrMoqErrorUrl
	// ErrTimeOverflow matches a timestamp that overflowed its timescale.
	ErrTimeOverflow = ffi.ErrMoqErrorTimeOverflow
	// ErrLogLevel matches an unparseable log level string passed to LogLevel.
	ErrLogLevel = ffi.ErrMoqErrorLogLevel
	// ErrTask matches a panic or cancellation in a background native task.
	ErrTask = ffi.ErrMoqErrorTask
	// ErrCancelled is returned when an operation is cancelled, e.g. via a cancelled context; IsShutdown treats it as a graceful stop.
	ErrCancelled = ffi.ErrMoqErrorCancelled
	// ErrClosed is returned when the session or stream has closed; IsShutdown treats it as a graceful stop.
	ErrClosed = ffi.ErrMoqErrorClosed
	// ErrConnect is returned when establishing a client session fails.
	ErrConnect = ffi.ErrMoqErrorConnect
	// ErrBind is returned when the server fails to bind its listening address.
	ErrBind = ffi.ErrMoqErrorBind
	// ErrReject is returned when a session is refused during the handshake.
	ErrReject = ffi.ErrMoqErrorReject
	// ErrAlreadyResponded is returned when a Request is accepted or rejected more than once.
	ErrAlreadyResponded = ffi.ErrMoqErrorAlreadyResponded
	// ErrCodec is returned when codec configuration or bitstream parsing fails.
	ErrCodec = ffi.ErrMoqErrorCodec
	// ErrUnauthorized is returned when the relay rejects the session with HTTP 401.
	ErrUnauthorized = ffi.ErrMoqErrorUnauthorized
	// ErrForbidden is returned when the relay rejects the session with HTTP 403.
	ErrForbidden = ffi.ErrMoqErrorForbidden
	// ErrNotFound is returned when the requested track or group is not available.
	ErrNotFound = ffi.ErrMoqErrorNotFound
	// ErrUnsupported is returned when the requested operation is not supported.
	ErrUnsupported = ffi.ErrMoqErrorUnsupported
	// ErrLog is returned when installing or configuring the native log subscriber fails.
	ErrLog = ffi.ErrMoqErrorLog
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
