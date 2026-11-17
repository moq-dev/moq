package moq

import (
	"reflect"
	"testing"
	"time"

	ffi "moq.dev/moq-ffi/moq"
)

func ptr[T any](v T) *T { return &v }

// Each bound reads nil as the native default, MaxUnlimited as no cap, and zero as zero,
// as moq-ffi's MoqBackoff does.
func TestBackoffFfiPassesEachBoundThrough(t *testing.T) {
	cases := []struct {
		name string
		in   Backoff
		want ffi.MoqBackoff
	}{
		{
			name: "zero value is the native default",
			in:   Backoff{},
			want: ffi.MoqBackoff{},
		},
		{
			name: "MaxUnlimited lifts the cap",
			in:   Backoff{MaxUnlimited: true},
			want: ffi.MoqBackoff{MaxUnlimited: true},
		},
		{
			name: "zero is zero",
			in:   Backoff{Initial: ptr(time.Duration(0)), Max: ptr(time.Duration(0)), Timeout: ptr(time.Duration(0))},
			want: ffi.MoqBackoff{Initial: ptr(time.Duration(0)), Max: ptr(time.Duration(0)), Timeout: ptr(time.Duration(0))},
		},
		{
			name: "explicit values pass through",
			in:   Backoff{Initial: ptr(500 * time.Millisecond), Multiplier: 3, Max: ptr(10 * time.Second), Timeout: ptr(time.Minute)},
			want: ffi.MoqBackoff{
				Initial:    ptr(500 * time.Millisecond),
				Multiplier: ptr[uint32](3),
				Max:        ptr(10 * time.Second),
				Timeout:    ptr(time.Minute),
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

// The generated converter panics on a negative duration, so it is refused first.
func TestBackoffFfiRejectsNegatives(t *testing.T) {
	for _, in := range []Backoff{{Initial: ptr(-time.Second)}, {Max: ptr(-time.Hour)}, {Timeout: ptr(-2 * time.Second)}} {
		if _, err := in.ffi(); err == nil {
			t.Errorf("Backoff%+v.ffi() succeeded, want an error", in)
		}
	}
}
