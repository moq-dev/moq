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
	if _, err := consumer.Request(ctx, "live", nil); !errors.Is(err, ErrClosed) {
		t.Fatalf("Request err = %v, want ErrClosed", err)
	}
}

// A prefix Route owns its origin: once the collector finalizes the last
// OriginProducer, the route still serves and a consumer made earlier still
// resolves through it.
func TestRouteKeepsTheOrigin(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	origin := NewOriginProducer()
	dynamic, err := origin.RoutePrefix("", RouteOptions{})
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
		broadcast, err := consumer.Request(ctx, "live", nil)
		requested <- result{broadcast: broadcast, err: err}
	}()

	request, err := dynamic.Requested(ctx)
	if err != nil || request == nil {
		t.Fatalf("Requested = (%v, %v), want a request", request, err)
	}
	served, err := request.Route(RouteOptions{})
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = served.broadcast.Close() }()

	var res result
	select {
	case res = <-requested:
	case <-ctx.Done():
		t.Fatal(ctx.Err())
	}
	if res.err != nil {
		t.Fatalf("Request err = %v, want the served broadcast", res.err)
	}
}

// Releasing a route, which is what its finalizer does, retracts it: the
// broadcast alone keeps nothing announced, though it lives on until closed.
func TestReleasingARouteRetractsIt(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	origin := NewOriginProducer()
	consumer := origin.Consume()
	route, err := origin.Route("live", RouteOptions{})
	if err != nil {
		t.Fatal(err)
	}
	broadcast, err := route.Broadcast()
	if err != nil {
		t.Fatal(err)
	}
	if again, _ := route.Broadcast(); again != broadcast {
		t.Fatal("Broadcast must return the same producer on every call")
	}
	if _, err := consumer.Request(ctx, "live", nil); err != nil {
		t.Fatalf("Request = %v, want the route's broadcast", err)
	}
	route.inner.Destroy()

	if _, err := consumer.Request(ctx, "live", nil); err == nil {
		t.Fatal("a released route must be unroutable")
	}
	if _, err := broadcast.PublishTrack("data", nil); err != nil {
		t.Fatalf("PublishTrack = %v, want the broadcast still open", err)
	}
	if err := broadcast.Close(); err != nil {
		t.Fatal(err)
	}
}
