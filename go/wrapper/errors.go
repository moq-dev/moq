package moq

import (
	"errors"

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
// callers can errors.Is against them without importing moq.dev/moq-ffi directly.
// These mirror the variants of the native error enum; transparent variants
// (Protocol, Media, ...) wrap a lower-level error whose detail survives in the
// message.
var (
	// ErrProtocol matches a session or stream protocol error; inspect ProtocolError for the code.
	ErrProtocol = ffi.ErrMoqErrorProtocol
	// ErrTransport matches a QUIC/WebTransport connection failure, not a protocol code.
	ErrTransport = ffi.ErrMoqErrorTransport
	// ErrInternal matches a local failure without a protocol code.
	ErrInternal = ffi.ErrMoqErrorInternal
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
	// ErrBusy is returned when CertFingerprints races an in-flight server operation.
	ErrBusy = ffi.ErrMoqErrorBusy
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
	// ErrAlreadyCommitted is returned when a track already reading one way (arrival order, sequence order, or a typed reader) is asked to read another; subscribe again for a second cursor.
	ErrAlreadyCommitted = ffi.ErrMoqErrorAlreadyCommitted
	// ErrInvalidRoute is returned when a route has an invalid hop ID or too many hops.
	ErrInvalidRoute = ffi.ErrMoqErrorInvalidRoute
	// ErrInvalidPattern is returned when a path pattern is empty, has a doubled slash, or uses a reserved segment form.
	ErrInvalidPattern = ffi.ErrMoqErrorInvalidPattern
	// ErrUnresolvableBroadcast is returned when a referenced sibling broadcast cannot be resolved without an origin.
	ErrUnresolvableBroadcast = ffi.ErrMoqErrorUnresolvableBroadcast
	// ErrLog is returned when installing or configuring the native log subscriber fails.
	ErrLog = ffi.ErrMoqErrorLog
	// ErrConfig is returned when Dial or Listen is given a value the native side cannot use.
	ErrConfig = ffi.ErrMoqErrorConfig
)

// IsShutdown reports whether err is the expected result of a graceful shutdown
// (Cancelled or Closed) rather than an actual failure. It's the value to check
// when a stream ends because its consumer was cancelled or the session closed.
func IsShutdown(err error) bool {
	return errors.Is(err, ErrCancelled) || errors.Is(err, ErrClosed)
}

// IsAuthError reports whether err is an authentication/authorization failure
// (HTTP 401/403, or a protocol Unauthorized session close).
func IsAuthError(err error) bool {
	if errors.Is(err, ErrUnauthorized) || errors.Is(err, ErrForbidden) {
		return true
	}
	protocol, ok := ProtocolError(err)
	return ok && protocol.Kind == ffi.MoqProtocolKindUnauthorized
}

// ProtocolError reports the structured protocol failure, if err is one.
//
// The record carries the peer's session or stream scope, the verbatim wire code,
// a known kind when recognized, and a diagnostic message.
func ProtocolError(err error) (ffi.MoqProtocolError, bool) {
	var proto *ffi.MoqErrorProtocol
	if !errors.As(err, &proto) || proto == nil {
		return ffi.MoqProtocolError{}, false
	}
	return proto.Details, true
}
