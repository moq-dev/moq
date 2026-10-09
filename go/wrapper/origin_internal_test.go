package moq

import (
	"context"
	"errors"
	"testing"
	"time"
)

// An OriginProducer has no Close: the collector ends its origin by finalizing
// the last owner, even while a consumer made from it is in use. Destroying the
// handle is what that finalizer does, so this pins the behavior the doc comment
// warns about without waiting on the collector.
func TestOriginEndsWithItsProducer(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	origin := NewOriginProducer()
	consumer := origin.Consume()
	announced, err := consumer.Announced(AnnounceOptions{})
	if err != nil {
		t.Fatal(err)
	}
	defer announced.Cancel()

	origin.inner.Destroy()

	// The teardown runs on the origin's driver; the cursor ending is its signal.
	update, err := announced.Next(ctx)
	if update != nil || err != nil {
		t.Fatalf("Next = (%v, %v), want the end of the stream", update, err)
	}
	if _, err := consumer.RequestBroadcast(ctx, "live"); !errors.Is(err, ErrClosed) {
		t.Fatalf("RequestBroadcast err = %v, want ErrClosed", err)
	}
}

// An OriginDynamic owns its origin: once the collector finalizes the last
// OriginProducer, the route still serves and a consumer made earlier still
// resolves through it.
func TestDynamicKeepsTheOrigin(t *testing.T) {
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

	type result struct {
		broadcast *BroadcastConsumer
		err       error
	}
	requested := make(chan result, 1)
	go func() {
		broadcast, err := consumer.RequestBroadcast(ctx, "live")
		requested <- result{broadcast: broadcast, err: err}
	}()

	request, err := dynamic.RequestedBroadcast(ctx)
	if err != nil {
		t.Fatalf("RequestedBroadcast err = %v, want a request", err)
	}
	served, err := NewBroadcastProducer()
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = served.Close() }()
	if err := request.Accept(served); err != nil {
		t.Fatal(err)
	}

	var res result
	select {
	case res = <-requested:
	case <-ctx.Done():
		t.Fatal(ctx.Err())
	}
	if res.err != nil {
		t.Fatalf("RequestBroadcast err = %v, want the served broadcast", res.err)
	}
}
