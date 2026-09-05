package moq

import (
	"context"
	"errors"
	"testing"
	"time"
)

// stubHandle stands in for a uniffi object: something the wrapper owns and has
// to destroy, whose disposal a test can observe.
type stubHandle struct {
	destroyed chan struct{}
}

func (s *stubHandle) Destroy() {
	close(s.destroyed)
}

// Cancelling is not a retraction. A call can produce a handle at the same moment
// its context expires, and the caller returns ctx.Err() with the handle already
// allocated; left to the Go finalizer it stays live in the meantime, which for a
// subscribe is a stream still running on the wire and for an Accept is an
// incoming request nobody answers. An expired context here is what a lost
// select race is downstream: a result nobody is waiting for.
func TestRunHandleReleasesAResultNobodyReceived(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	cancel()

	stub := &stubHandle{destroyed: make(chan struct{})}
	finish := make(chan struct{})

	val, err := runHandle(ctx, nil, func() (*stubHandle, error) {
		<-finish
		return stub, nil
	})
	if !errors.Is(err, context.Canceled) {
		t.Fatalf("err = %v, want context.Canceled", err)
	}
	if val != nil {
		t.Fatalf("val = %v, want nil", val)
	}

	// The call completes after the caller has gone, which is the case that has
	// nobody left to hand the handle to.
	close(finish)
	select {
	case <-stub.destroyed:
	case <-time.After(5 * time.Second):
		t.Fatal("the abandoned handle was never destroyed")
	}
}

// A successful call can still yield no handle: Accept answers nil once the
// server has stopped. Destroying that is a nil dereference, so the release has
// to skip it.
func TestRunHandleIgnoresAResultThatIsNoHandle(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	cancel()

	done := make(chan struct{})
	_, err := runHandle(ctx, nil, func() (*stubHandle, error) {
		defer close(done)
		return nil, nil
	})
	if !errors.Is(err, context.Canceled) {
		t.Fatalf("err = %v, want context.Canceled", err)
	}

	<-done
	// The release runs on its own goroutine, so a panic there takes the process
	// down rather than failing here; give it a moment to happen.
	time.Sleep(50 * time.Millisecond)
}

// A call that beats its context keeps its result: the reconciliation must not
// cost the caller a handle it did receive.
func TestRunHandleReturnsAResultThatWon(t *testing.T) {
	stub := &stubHandle{destroyed: make(chan struct{})}

	val, err := runHandle(context.Background(), nil, func() (*stubHandle, error) {
		return stub, nil
	})
	if err != nil {
		t.Fatal(err)
	}
	if val != stub {
		t.Fatalf("val = %v, want the handle the call returned", val)
	}
	select {
	case <-stub.destroyed:
		t.Fatal("a handle handed to the caller was destroyed")
	default:
	}
}
