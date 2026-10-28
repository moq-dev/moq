package moq

import (
	"reflect"
	"testing"
	"time"

	ffi "moq.dev/moq-ffi/moq"
)

// Go's zero values mean "unset" to a caller, but an empty string, zero stream cap,
// or zero duration means something else (or is refused) natively. Only nil and the
// empty values map to the native null; an explicit zero duration passes through.
func TestClientConfigFfiMapsZeroValuesToNativeDefaults(t *testing.T) {
	got, err := ClientConfig{}.ffi()
	if err != nil {
		t.Fatalf("ClientConfig{}.ffi() failed: %v", err)
	}
	// The wrapper's own product token is the one field it always sets.
	if !reflect.DeepEqual(got, ffi.MoqClientConfig{Implementation: &implementation}) {
		t.Errorf("ClientConfig{}.ffi() = %+v, want the native defaults", got)
	}

	got, err = ClientConfig{Timeout: ptr(time.Duration(0)), QUIC: QUICConfig{KeepAlive: ptr(time.Duration(0))}}.ffi()
	if err != nil {
		t.Fatalf("ffi() failed: %v", err)
	}
	if got.Timeout == nil || *got.Timeout != 0 || got.Quic.KeepAlive == nil || *got.Quic.KeepAlive != 0 {
		t.Errorf("an explicit zero duration was dropped: timeout %v, keep-alive %v", got.Timeout, got.Quic.KeepAlive)
	}

	if _, err := (ClientConfig{Race: ptr(-time.Millisecond)}).ffi(); err == nil {
		t.Error("a negative race converted, want an error")
	}
}
