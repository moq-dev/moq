package moq

import (
	"context"
	"errors"
	"testing"
	"time"
)

// An OriginProducer has no Close: the collector ends its origin by finalizing
// the last producer, even while a consumer or dynamic handle made from it is in
// use. Destroying the handle is what that finalizer does, so this pins the
// behavior the doc comment warns about without waiting on the collector.
func TestOriginEndsWithItsProducer(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	origin := NewOriginProducer()
	dynamic, err := origin.Dynamic("", Route{})
	if err != nil {
		t.Fatal(err)
	}
	defer dynamic.Cancel()
	consumer := origin.Consume()

	origin.inner.Destroy()

	if _, err := dynamic.RequestedBroadcast(ctx); !errors.Is(err, ErrClosed) {
		t.Fatalf("RequestedBroadcast err = %v, want ErrClosed", err)
	}
	if _, err := consumer.RequestBroadcast(ctx, "live"); !errors.Is(err, ErrClosed) {
		t.Fatalf("RequestBroadcast err = %v, want ErrClosed", err)
	}
}
