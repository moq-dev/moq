package moq

import (
	"reflect"
	"testing"
	"time"

	ffi "moq.dev/moq-ffi/moq"
)

func ptr[T any](v T) *T { return &v }

// The conversion is where Go's zero value meets the native encoding, and the two
// disagree: zero means "unset" to a Go caller but "no delay" and "retry forever"
// to the reconnect loop. Passing it through as zero turns the most natural
// literal a caller writes, Backoff{}, into an unthrottled dial loop.
func TestBackoffFfiLeavesUnsetFieldsToTheNativeDefaults(t *testing.T) {
	cases := []struct {
		name string
		in   Backoff
		want ffi.MoqBackoff
	}{
		{
			name: "zero value is the native default, never an unpaced loop",
			in:   Backoff{},
			want: ffi.MoqBackoff{},
		},
		{
			name: "a partial override keeps the defaults for everything else",
			in:   Backoff{Max: time.Second},
			want: ffi.MoqBackoff{MaxUs: ptr[uint64](1_000_000)},
		},
		{
			name: "RetryForever is the only way to reach the native zero timeout",
			in:   Backoff{Timeout: RetryForever},
			want: ffi.MoqBackoff{TimeoutUs: ptr[uint64](0)},
		},
		{
			name: "a sub-microsecond delay floors at 1us instead of truncating to zero",
			in:   Backoff{Initial: time.Nanosecond},
			want: ffi.MoqBackoff{InitialUs: ptr[uint64](1)},
		},
		{
			name: "explicit values pass through",
			in:   Backoff{Initial: 500 * time.Millisecond, Multiplier: 3, Max: 10 * time.Second, Timeout: time.Minute},
			want: ffi.MoqBackoff{
				InitialUs:  ptr[uint64](500_000),
				Multiplier: ptr[uint32](3),
				MaxUs:      ptr[uint64](10_000_000),
				TimeoutUs:  ptr[uint64](60_000_000),
			},
		},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			got, err := tc.in.ffi()
			if err != nil {
				t.Fatalf("Backoff%+v.ffi() failed: %v", tc.in, err)
			}
			if !reflect.DeepEqual(got, tc.want) {
				t.Errorf("Backoff%+v.ffi() = %+v, want %+v", tc.in, got, tc.want)
			}
		})
	}
}

// A negative duration would wrap to ~1.8e19 us when cast, so it is refused.
func TestBackoffFfiRejectsNegatives(t *testing.T) {
	for _, in := range []Backoff{{Initial: -time.Second}, {Max: -time.Hour}, {Timeout: -2 * time.Second}} {
		if _, err := in.ffi(); err == nil {
			t.Errorf("Backoff%+v.ffi() succeeded, want an error", in)
		}
	}
}
