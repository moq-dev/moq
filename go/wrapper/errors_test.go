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
	if err != nil {
		t.Fatal(err)
	}
	track, err := broadcast.PublishTrack("errors", nil)
	if err != nil {
		t.Fatal(err)
	}
	producer, err := track.AppendGroup()
	if err != nil {
		t.Fatal(err)
	}
	consumer, err := broadcast.Consume()
	if err != nil {
		t.Fatal(err)
	}
	group, err := consumer.FetchGroup(ctx, "errors", 0, nil)
	if err != nil {
		t.Fatal(err)
	}
	if err := producer.Abort(404); err != nil {
		t.Fatal(err)
	}
	_, err = group.ReadFrame(ctx)
	details, ok := moq.ProtocolError(err)
	if !ok || details.Scope != moq.ErrorScopeStream || details.Code != 468 || details.Kind != moq.ProtocolKindApp {
		t.Fatalf("lost protocol details: %+v, %v", details, err)
	}
}

func TestErrorMessage(t *testing.T) {
	tests := []struct {
		err  error
		want string
	}{
		{ffi.NewMoqErrorClosed(), "closed"},
		{ffi.NewMoqErrorTransport("reset"), "transport: reset"},
		{ffi.NewMoqErrorProtocol(ffi.MoqProtocolError{
			Scope:   ffi.MoqErrorScopeStream,
			Code:    468,
			Kind:    ffi.MoqProtocolKindApp,
			Message: "gone",
		}), "gone"},
	}
	for _, test := range tests {
		if got := test.err.Error(); got != test.want {
			t.Errorf("Error() = %q, want %q", got, test.want)
		}
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
		{"Busy", ffi.NewMoqErrorBusy(), moq.ErrBusy},
		{"Connect", ffi.NewMoqErrorConnect(""), moq.ErrConnect},
		{"Bind", ffi.NewMoqErrorBind(""), moq.ErrBind},
		{"Reject", ffi.NewMoqErrorReject(""), moq.ErrReject},
		{"AlreadyResponded", ffi.NewMoqErrorAlreadyResponded(), moq.ErrAlreadyResponded},
		{"Codec", ffi.NewMoqErrorCodec(""), moq.ErrCodec},
		{"Unauthorized", ffi.NewMoqErrorUnauthorized(), moq.ErrUnauthorized},
		{"Forbidden", ffi.NewMoqErrorForbidden(), moq.ErrForbidden},
		{"Expired", ffi.NewMoqErrorExpired(), moq.ErrExpired},
		{"NotFound", ffi.NewMoqErrorNotFound(), moq.ErrNotFound},
		{"Unsupported", ffi.NewMoqErrorUnsupported(), moq.ErrUnsupported},
		{"InvalidPattern", ffi.NewMoqErrorInvalidPattern(""), moq.ErrInvalidPattern},
		{"InvalidEpoch", ffi.NewMoqErrorInvalidEpoch(""), moq.ErrInvalidEpoch},
		{"UnresolvableBroadcast", ffi.NewMoqErrorUnresolvableBroadcast(""), moq.ErrUnresolvableBroadcast},
		{"UnsupportedCatalog", ffi.NewMoqErrorUnsupportedCatalog(5), moq.ErrUnsupportedCatalog},
		{"AlreadyCommitted", ffi.NewMoqErrorAlreadyCommitted(), moq.ErrAlreadyCommitted},
		{"Log", ffi.NewMoqErrorLog(""), moq.ErrLog},
		{"Config", ffi.NewMoqErrorConfig(""), moq.ErrConfig},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			if !errors.Is(test.err, test.sentinel) {
				t.Fatalf("errors.Is(%v, %v) = false", test.err, test.sentinel)
			}
		})
	}
}

func TestSessionLimitProtocolKind(t *testing.T) {
	err := ffi.NewMoqErrorProtocol(ffi.MoqProtocolError{
		Scope: moq.ErrorScopeSession, Code: 7, Kind: moq.ProtocolKindTooManyRequests, Message: "request limit",
	})
	details, ok := moq.ProtocolError(err)
	if !ok || details.Kind != moq.ProtocolKindTooManyRequests || moq.IsAuthError(err) {
		t.Fatalf("session limit lost its kind: %+v, %v", details, err)
	}
}

func TestExpiredIsAuthError(t *testing.T) {
	err := ffi.NewMoqErrorProtocol(ffi.MoqProtocolError{
		Scope: moq.ErrorScopeSession, Code: 0x18, Kind: moq.ProtocolKindExpired, Message: "expired",
	})
	if !moq.IsAuthError(err) {
		t.Fatalf("expired credentials are not an auth error: %v", err)
	}
	if !moq.IsAuthError(ffi.NewMoqErrorExpired()) {
		t.Fatal("an expired request token is not an auth error")
	}
}

func TestTrackRevocationIsAuth(t *testing.T) {
	err := ffi.NewMoqErrorProtocol(ffi.MoqProtocolError{
		Scope: moq.ErrorScopeStream, Code: 0x3b, Kind: moq.ProtocolKindUnauthorized, Message: "unauthorized",
	})
	if !moq.IsAuthError(err) {
		t.Fatalf("a track revocation is not an auth error: %v", err)
	}
}
