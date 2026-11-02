package moq

import (
	"math"
	"testing"
	"time"
)

// A duration converts at its own nanosecond scale, so nothing rounds on the way in.
func TestTimestampFromDuration(t *testing.T) {
	got, err := TimestampFromDuration(1500 * time.Millisecond)
	if err != nil {
		t.Fatal(err)
	}
	if want := (Timestamp{Value: 1_500_000_000, Scale: 1_000_000_000}); got != want {
		t.Fatalf("TimestampFromDuration(1.5s) = %+v, want %+v", got, want)
	}
	if got := TimestampToDuration(got); got != 1500*time.Millisecond {
		t.Fatalf("round trip = %v, want 1.5s", got)
	}
	if _, err := TimestampFromDuration(-time.Nanosecond); err == nil {
		t.Fatal("negative duration converted")
	}
}

// Converting to a duration rounds down to a whole nanosecond, and a time past
// time.Duration's ~292 years reads as the longest duration rather than wrapping.
func TestTimestampToDuration(t *testing.T) {
	cases := []struct {
		in   Timestamp
		want time.Duration
	}{
		{Timestamp{Value: 0, Scale: 90_000}, 0},
		// 11111.1 ns, 666666666.6 ns, and half a nanosecond all round down.
		{Timestamp{Value: 1, Scale: 90_000}, 11_111},
		{Timestamp{Value: 2, Scale: 3}, 666_666_666},
		{Timestamp{Value: 1, Scale: 2_000_000_000}, 0},
		{Timestamp{Value: 1001, Scale: 30_000}, 33_366_666},
		{Timestamp{Value: math.MaxInt64, Scale: 1_000_000_000}, math.MaxInt64},
		{Timestamp{Value: math.MaxInt64/1000 + 1, Scale: 1_000_000}, math.MaxInt64},
		{Timestamp{Value: 1<<61 - 1, Scale: 1}, math.MaxInt64},
		{Timestamp{Value: math.MaxUint64, Scale: math.MaxUint64}, time.Second},
	}
	for _, c := range cases {
		if got := TimestampToDuration(c.in); got != c.want {
			t.Errorf("TimestampToDuration(%+v) = %d, want %d", c.in, got, c.want)
		}
	}
}

// A zero scale, such as an unset Timestamp{}, panics rather than reading as the
// longest duration.
func TestTimestampToDurationZeroScale(t *testing.T) {
	defer func() {
		if recover() == nil {
			t.Fatal("TimestampToDuration(Timestamp{}) did not panic")
		}
	}()
	TimestampToDuration(Timestamp{})
}

// DefaultSubscription carries moq-net's zero max delay, while the zero Subscription
// leaves it nil, which sets no subscriber limit.
func TestDefaultSubscriptionMaxDelay(t *testing.T) {
	got, err := DefaultSubscription().ffi()
	if err != nil {
		t.Fatal(err)
	}
	if got.MaxDelay == nil || *got.MaxDelay != 0 {
		t.Errorf("DefaultSubscription MaxDelay = %v, want 0", got.MaxDelay)
	}
	got, err = Subscription{}.ffi()
	if err != nil {
		t.Fatal(err)
	}
	if got.MaxDelay != nil {
		t.Errorf("zero Subscription MaxDelay = %v, want nil", *got.MaxDelay)
	}
}
