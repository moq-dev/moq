package moq

import (
	"math"
	"reflect"
	"testing"
	"time"
)

// A duration converts at its own nanosecond scale, so nothing rounds on the way in.
func TestTimestampFromDuration(t *testing.T) {
	got, err := TimestampFromDuration(1500 * time.Millisecond)
	if err != nil {
		t.Fatal(err)
	}
	if want := TimestampFromScale(1_500_000_000, 1_000_000_000); !reflect.DeepEqual(got, want) {
		t.Fatalf("TimestampFromDuration(1.5s) = %+v, want %+v", got, want)
	}
	if got, err := TimestampToDuration(got); err != nil || got != 1500*time.Millisecond {
		t.Fatalf("round trip = %v, %v, want 1.5s", got, err)
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
		{TimestampFromScale(0, 90_000), 0},
		// 11111.1 ns, 666666666.6 ns, and half a nanosecond all round down.
		{TimestampFromScale(1, 90_000), 11_111},
		{TimestampFromScale(2, 3), 666_666_666},
		{TimestampFromScale(1, 2_000_000_000), 0},
		{TimestampFromScale(1001, 30_000), 33_366_666},
		{TimestampFromScale(math.MaxInt64, 1_000_000_000), math.MaxInt64},
		{TimestampFromScale(math.MaxInt64/1000+1, 1_000_000), math.MaxInt64},
		{TimestampFromScale(1<<61-1, 1), math.MaxInt64},
		{TimestampFromScale(math.MaxUint64, math.MaxUint64), time.Second},
	}
	for _, c := range cases {
		if got, err := TimestampToDuration(c.in); err != nil || got != c.want {
			t.Errorf("TimestampToDuration(%+v) = %d, %v, want %d", c.in, got, err, c.want)
		}
	}
}

// An unscaled timestamp has no time, and a zero scale no duration, so both fail rather
// than reading as some duration.
func TestTimestampToDurationRefused(t *testing.T) {
	for _, in := range []Timestamp{Unscaled(7), Unscaled(0), TimestampFromScale(1, 0)} {
		if got, err := TimestampToDuration(in); err == nil {
			t.Errorf("TimestampToDuration(%+v) = %v, want an error", in, got)
		}
	}
	if got := Unscaled(7); got.Scale != nil || got.Value != 7 {
		t.Fatalf("Unscaled(7) = %+v", got)
	}
}

// The zero Subscription is moq-net's defaults, a stated zero Delay, and the flags reach the
// unstated minimum and the unlimited max.
func TestSubscriptionDelay(t *testing.T) {
	if got := DefaultSubscription(); !reflect.DeepEqual(got, Subscription{}) {
		t.Errorf("DefaultSubscription() = %+v, want the zero Subscription", got)
	}
	got, err := Subscription{}.ffi()
	if err != nil {
		t.Fatal(err)
	}
	if got.Delay.Min == nil || *got.Delay.Min != 0 || got.Delay.Max == nil || *got.Delay.Max != 0 {
		t.Errorf("zero Subscription Delay = %+v, want a stated zero for both", got.Delay)
	}
	got, err = Subscription{Delay: Delay{Min: time.Second, MinUnstated: true, Max: time.Second, MaxUnlimited: true}}.ffi()
	if err != nil {
		t.Fatal(err)
	}
	if got.Delay.Min != nil || got.Delay.Max != nil {
		t.Errorf("flagged Delay = %+v, want neither stated", got.Delay)
	}
	if _, err := (Subscription{Delay: Delay{Max: -time.Second}}).ffi(); err == nil {
		t.Error("a negative max delay converted")
	}
}
