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
