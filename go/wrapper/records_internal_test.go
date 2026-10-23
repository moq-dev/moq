package moq

import (
	"math"
	"testing"
	"time"
)

// A peer's delay or max age can pass time.Duration's ~292 years, which must read as
// the longest duration rather than wrap to a negative one that is already expired.
func TestFromMicrosSaturates(t *testing.T) {
	limit := uint64(math.MaxInt64 / int64(time.Microsecond))
	cases := []struct {
		us   uint64
		want time.Duration
	}{
		{us: 1, want: time.Microsecond},
		{us: limit, want: time.Duration(limit) * time.Microsecond},
		{us: limit + 1, want: time.Duration(math.MaxInt64)},
		{us: math.MaxUint64, want: time.Duration(math.MaxInt64)},
	}
	for _, c := range cases {
		if got := fromMicros(c.us); got != c.want {
			t.Errorf("fromMicros(%d) = %v, want %v", c.us, got, c.want)
		}
	}
}
