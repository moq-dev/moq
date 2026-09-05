package moq

import (
	"context"
	"errors"
	"sync"

	ffi "moq.dev/moq-ffi/moq"
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
	// ErrJSONTrack matches a JSON track encoding or decoding failure.
	ErrJSONTrack = ffi.ErrMoqErrorJsonTrack
	// ErrAudio matches a raw-audio encode or decode failure.
	ErrAudio = ffi.ErrMoqErrorAudio
	// ErrVideo matches a raw-video encode or decode failure.
	ErrVideo = ffi.ErrMoqErrorVideo
	// ErrURL matches a malformed URL passed when connecting or publishing.
	ErrURL = ffi.ErrMoqErrorUrl
	// ErrTimeOverflow matches a timestamp that overflowed its timescale.
	ErrTimeOverflow = ffi.ErrMoqErrorTimeOverflow
	// ErrLogLevel matches an unparseable log level string passed to LogLevel.
	ErrLogLevel = ffi.ErrMoqErrorLogLevel
	// ErrTask matches a panic or cancellation in a background native task.
	ErrTask = ffi.ErrMoqErrorTask
	// ErrJSON matches malformed JSON passed through the FFI API.
	ErrJSON = ffi.ErrMoqErrorJson
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
	// ErrUnsupported is returned when the requested operation is not supported by this build or peer, however it is asked for.
	ErrUnsupported = ffi.ErrMoqErrorUnsupported
	// ErrAlreadyCommitted is returned when a track already reading in one group order is asked for the other; subscribe again for a second cursor.
	ErrAlreadyCommitted = ffi.ErrMoqErrorAlreadyCommitted
	// ErrInvalidRoute is returned when a route has an invalid hop ID or too many hops.
	ErrInvalidRoute = ffi.ErrMoqErrorInvalidRoute
	// ErrUnresolvableBroadcast is returned when a referenced sibling broadcast cannot be resolved without an origin.
	ErrUnresolvableBroadcast = ffi.ErrMoqErrorUnresolvableBroadcast
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

// handle is a native object the wrapper owns. Every uniffi-generated object has
// Destroy, which drops the Rust-side Arc, and comparable so an optional one can
// be checked for nil before it is destroyed.
type handle interface {
	comparable
	Destroy()
}

// run races a blocking FFI call against ctx.
//
// uniffi-bindgen-go renders Rust async fns as blocking Go calls with no context
// parameter, so cancellation is wired by calling cancel (which aborts the
// in-flight native task) when ctx is done. The blocked goroutine then unwinds on
// its own; the result channel is buffered so its send never blocks and it can't
// leak.
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

// runCancellable runs a blocking FFI call that yields no handle of its own, so a
// result the caller never sees costs nothing to drop.
//
// cancel is the object's own cancel() for a call that owns its stream, and a
// per-call token for everything else. See runOperation.
func runCancellable[T any](ctx context.Context, cancel func(), call func() (T, error)) (T, error) {
	return run(ctx, cancel, call, nil)
}

// runHandle is runCancellable for a call that returns a native handle, which is
// destroyed rather than abandoned when the caller has already given up. Left to
// the Go finalizer it would stay live in the meantime: a subscription still
// running on the wire, or an incoming request accepted and never answered.
func runHandle[T handle](ctx context.Context, cancel func(), call func() (T, error)) (T, error) {
	return run(ctx, cancel, call, releaseHandle[T])
}

// runErr is runCancellable for calls that return only an error.
func runErr(ctx context.Context, cancel func(), call func() error) error {
	_, err := runCancellable(ctx, cancel, func() (struct{}, error) {
		return struct{}{}, call()
	})
	return err
}

// token is the per-call cancellation object plus the synchronization its two
// users need. `cancel` runs on the caller's goroutine and `release` on the one
// running the call, and the generated bindings panic on any use of a destroyed
// object, so the two are serialized and only one of them ever frees it.
//
// Releasing on the call's own goroutine is what makes the object outlive every
// use of it: the call cannot start after its own deferred release, and a cancel
// that arrives later finds the token already gone and does nothing. Nothing else
// may release it, since a token freed while its goroutine is between spawn and
// first use would panic that goroutine.
//
// So a token is minted only once its call is going to start, which is why the
// callers check the context first. A context that becomes done in the window
// between that check and `run`'s leaves one token to the finalizer, which is
// what a finalizer is for; what it cannot do is accumulate, since every
// subsequent call with that context mints nothing at all.
type token struct {
	// Set once at construction and never reassigned, so the call can read it
	// without the lock.
	inner *ffi.MoqCancel

	mu       sync.Mutex
	released bool
}

func newToken() *token {
	return &token{inner: ffi.NewMoqCancel()}
}

// cancel aborts the call this token was minted for, or does nothing once that
// call has returned and released it.
func (t *token) cancel() {
	t.mu.Lock()
	defer t.mu.Unlock()
	if !t.released {
		t.inner.Cancel()
	}
}

// release frees the native object, once. Without it a run of quick calls piles
// tokens up until the Go finalizer notices them.
func (t *token) release() {
	t.mu.Lock()
	defer t.mu.Unlock()
	if !t.released {
		t.released = true
		t.inner.Destroy()
	}
}

// runOperation runs a blocking FFI call that takes a per-call cancellation token
// and returns a native handle.
//
// Cancelling ctx aborts that one call and leaves the object it was made on usable,
// which is what the object-wide cancel() can't express: a deadline on a subscribe
// must not close the broadcast, and one on Accept must not close the server. The
// native task unwinds with the token, so nothing outlives the caller that gave up.
func runOperation[T handle](ctx context.Context, call func(*ffi.MoqCancel) (T, error)) (T, error) {
	// Mint nothing for a context already done: `run` starts no call, so the
	// closure that would release the token never runs. See `token`.
	if err := ctx.Err(); err != nil {
		var zero T
		return zero, err
	}

	tok := newToken()
	return runHandle(ctx, tok.cancel, func() (T, error) {
		defer tok.release()
		return call(tok.inner)
	})
}

// runOperationErr is runOperation for calls that return only an error.
func runOperationErr(ctx context.Context, call func(*ffi.MoqCancel) error) error {
	if err := ctx.Err(); err != nil {
		return err
	}

	tok := newToken()
	return runErr(ctx, tok.cancel, func() error {
		defer tok.release()
		return call(tok.inner)
	})
}

// releaseHandle drops a handle the caller never received. A successful call can
// still yield none (an Accept once the server has stopped), which is the zero
// value rather than something to destroy.
func releaseHandle[T handle](val T) {
	var zero T
	if val != zero {
		val.Destroy()
	}
}
