package moq_test

import (
	"context"
	"errors"
	"testing"

	"moq.dev/moq"
	ffi "moq.dev/moq-ffi/moq"
)

func TestStreamAbortProtocolDetails(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), testTimeout)
	defer cancel()
	broadcast, err := moq.NewBroadcastProducer()
	if err != nil { t.Fatal(err) }
	track, err := broadcast.PublishTrack("errors", nil)
	if err != nil { t.Fatal(err) }
	producer, err := track.AppendGroup()
	if err != nil { t.Fatal(err) }
	consumer, err := broadcast.Consume()
	if err != nil { t.Fatal(err) }
	group, err := consumer.FetchGroup(ctx, "errors", 0, nil)
	if err != nil { t.Fatal(err) }
	if err := producer.Abort(404); err != nil { t.Fatal(err) }
	_, err = group.ReadFrame(ctx)
	details, ok := moq.ProtocolError(err)
	if !ok || details.Scope != moq.ErrorScopeStream || details.Code != 468 || details.Kind != moq.ProtocolKindApp {
		t.Fatalf("lost protocol details: %+v, %v", details, err)
	}
}

func TestErrorSentinels(t *testing.T) {
	tests := []struct {
		name     string
		err      error
		sentinel error
	}{
		{"Protocol", ffi.NewMoqErrorProtocol(ffi.MoqProtocolError{}), moq.ErrProtocol},
		{"Internal", ffi.NewMoqErrorInternal(""), moq.ErrInternal},
		{"Transport", ffi.NewMoqErrorTransport(""), moq.ErrTransport},
		{"Media", ffi.NewMoqErrorMedia(""), moq.ErrMedia},
		{"Mux", ffi.NewMoqErrorMux(""), moq.ErrMux},
		{"JsonTrack", ffi.NewMoqErrorJsonTrack(""), moq.ErrJSONTrack},
		{"Audio", ffi.NewMoqErrorAudio(""), moq.ErrAudio},
		{"Video", ffi.NewMoqErrorVideo(""), moq.ErrVideo},
		{"Url", ffi.NewMoqErrorUrl(""), moq.ErrURL},
		{"TimeOverflow", ffi.NewMoqErrorTimeOverflow(), moq.ErrTimeOverflow},
		{"LogLevel", ffi.NewMoqErrorLogLevel(""), moq.ErrLogLevel},
		{"Task", ffi.NewMoqErrorTask(""), moq.ErrTask},
		{"Json", ffi.NewMoqErrorJson(""), moq.ErrJSON},
		{"Cancelled", ffi.NewMoqErrorCancelled(), moq.ErrCancelled},
		{"Closed", ffi.NewMoqErrorClosed(), moq.ErrClosed},
		{"Connect", ffi.NewMoqErrorConnect(""), moq.ErrConnect},
		{"Bind", ffi.NewMoqErrorBind(""), moq.ErrBind},
		{"Reject", ffi.NewMoqErrorReject(""), moq.ErrReject},
		{"AlreadyResponded", ffi.NewMoqErrorAlreadyResponded(), moq.ErrAlreadyResponded},
		{"Codec", ffi.NewMoqErrorCodec(""), moq.ErrCodec},
		{"Unauthorized", ffi.NewMoqErrorUnauthorized(), moq.ErrUnauthorized},
		{"Forbidden", ffi.NewMoqErrorForbidden(), moq.ErrForbidden},
		{"NotFound", ffi.NewMoqErrorNotFound(), moq.ErrNotFound},
		{"Unsupported", ffi.NewMoqErrorUnsupported(), moq.ErrUnsupported},
		{"InvalidRoute", ffi.NewMoqErrorInvalidRoute(""), moq.ErrInvalidRoute},
		{"InvalidPattern", ffi.NewMoqErrorInvalidPattern(""), moq.ErrInvalidPattern},
		{"UnresolvableBroadcast", ffi.NewMoqErrorUnresolvableBroadcast(""), moq.ErrUnresolvableBroadcast},
		{"AlreadyCommitted", ffi.NewMoqErrorAlreadyCommitted(), moq.ErrAlreadyCommitted},
		{"Log", ffi.NewMoqErrorLog(""), moq.ErrLog},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			if !errors.Is(test.err, test.sentinel) {
				t.Fatalf("errors.Is(%v, %v) = false", test.err, test.sentinel)
			}
		})
	}
}
