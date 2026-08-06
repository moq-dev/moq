package moq

import (
	"testing"
	"time"

	ffi "github.com/moq-dev/moq-go-ffi/moq"
)

// The conversion is where Go's zero value meets the native encoding, and the two
// disagree: zero means "unset" to a Go caller but "no delay" and "retry forever"
// to the reconnect loop. Passing it through unresolved turns the most natural
// literal a caller writes, Backoff{}, into an unthrottled dial loop.
func TestBackoffFfiResolvesUnsetFields(t *testing.T) {
	defaults := ffi.MoqBackoff{InitialMs: 1000, Multiplier: 2, MaxMs: 30000, TimeoutMs: 300000}

	cases := []struct {
		name string
		in   Backoff
		want ffi.MoqBackoff
	}{
		{
			name: "zero value is the documented default, never an unpaced loop",
			in:   Backoff{},
			want: defaults,
		},
		{
			name: "a partial override keeps the defaults for everything else",
			in:   Backoff{Max: 5 * time.Second},
			want: ffi.MoqBackoff{InitialMs: 1000, Multiplier: 2, MaxMs: 5000, TimeoutMs: 300000},
		},
		{
			name: "RetryForever is the only way to reach the native zero timeout",
			in:   Backoff{Timeout: RetryForever},
			want: ffi.MoqBackoff{InitialMs: 1000, Multiplier: 2, MaxMs: 30000, TimeoutMs: 0},
		},
		{
			name: "negatives fall back instead of wrapping to ~1.8e19 ms",
			in:   Backoff{Initial: -time.Second, Max: -time.Hour},
			want: defaults,
		},
		{
			name: "a sub-millisecond delay floors at 1ms instead of truncating to zero",
			in:   Backoff{Initial: time.Nanosecond},
			want: ffi.MoqBackoff{InitialMs: 1, Multiplier: 2, MaxMs: 30000, TimeoutMs: 300000},
		},
		{
			name: "explicit values pass through",
			in:   Backoff{Initial: 500 * time.Millisecond, Multiplier: 3, Max: 10 * time.Second, Timeout: time.Minute},
			want: ffi.MoqBackoff{InitialMs: 500, Multiplier: 3, MaxMs: 10000, TimeoutMs: 60000},
		},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			if got := tc.in.ffi(); got != tc.want {
				t.Errorf("Backoff%+v.ffi() = %+v, want %+v", tc.in, got, tc.want)
			}
		})
	}
}
