package bridge

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
// incoming request nobody answers.
//
// The call parks until the context is cancelled, so the deadline branch wins
// without a race, and only then produces the handle nobody is left to receive.
// Only an object-owned call is abandoned this way; one without cancel waits.
func TestRunHandleReleasesAResultNobodyReceived(t *testing.T) {
	ctx, cancelCtx := context.WithCancel(context.Background())
	started := make(chan struct{})
	finish := make(chan struct{})
	stub := &stubHandle{destroyed: make(chan struct{})}

	go func() {
		<-started
		cancelCtx()
	}()

	val, err := CallHandle(ctx, func() {}, func(context.Context) (*stubHandle, error) {
		close(started)
		<-finish
		return stub, nil
	})
	if !errors.Is(err, context.Canceled) {
		t.Fatalf("err = %v, want context.Canceled", err)
	}
	if val != nil {
		t.Fatalf("val = %v, want nil", val)
	}

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
	ctx, cancelCtx := context.WithCancel(context.Background())
	started := make(chan struct{})
	finish := make(chan struct{})

	go func() {
		<-started
		cancelCtx()
	}()

	// CallHandle's own release, wrapped to report when it has run. It runs on its
	// own goroutine, so a nil dereference there takes the process down rather
	// than failing here.
	released := make(chan struct{})
	_, err := run(ctx, func() {}, func() (*stubHandle, error) {
		close(started)
		<-finish
		return nil, nil
	}, func(val *stubHandle) {
		releaseHandle(val)
		close(released)
	})
	if !errors.Is(err, context.Canceled) {
		t.Fatalf("err = %v, want context.Canceled", err)
	}

	close(finish)
	<-released
}

// A cancelled read without an object cancel returns only once the call has: the
// generated binding owns that cancellation, and a read left running would take
// the next item from the stream and lose it.
func TestRunWaitsForACallWithoutObjectCancel(t *testing.T) {
	ctx, cancelCtx := context.WithCancel(context.Background())
	returned := false

	_, err := Call(ctx, nil, func(callCtx context.Context) (int, error) {
		cancelCtx()
		<-callCtx.Done()
		returned = true
		return 0, callCtx.Err()
	})
	if !errors.Is(err, context.Canceled) {
		t.Fatalf("err = %v, want context.Canceled", err)
	}
	if !returned {
		t.Fatal("Call returned before the cancelled call did")
	}
}

// A context that is already done starts no call at all, so a request that would
// have resolved immediately never reaches the native side.
func TestRunHandleStartsNothingForADoneContext(t *testing.T) {
	ctx, cancelCtx := context.WithCancel(context.Background())
	cancelCtx()

	called := false
	val, err := CallHandle(ctx, nil, func(context.Context) (*stubHandle, error) {
		called = true
		return &stubHandle{destroyed: make(chan struct{})}, nil
	})
	if !errors.Is(err, context.Canceled) {
		t.Fatalf("err = %v, want context.Canceled", err)
	}
	if val != nil {
		t.Fatalf("val = %v, want nil", val)
	}
	if called {
		t.Fatal("the call ran for a context that was already done")
	}
}

// Object-owned calls must not share the caller's cancellation with the generated
// binding. If they do, the binding can return ctx.Err() into the result channel
// before run invokes cancel, and ReadFrame / Closed / Accept report cancellation
// while leaving the stream, session, or request active.
func TestRunHidesCallerCancelFromAnObjectOwnedCall(t *testing.T) {
	ctx, cancelCtx := context.WithCancel(context.Background())
	defer cancelCtx()

	got := make(chan context.Context, 1)
	_, err := Call(ctx, func() {}, func(callCtx context.Context) (int, error) {
		got <- callCtx
		return 1, nil
	})
	if err != nil {
		t.Fatal(err)
	}
	callCtx := <-got
	cancelCtx()
	if callCtx.Err() != nil {
		t.Fatal("object-owned generated call shares the caller's cancellation")
	}
}

// A call with no object to abort still sees ctx, so one-shot generated methods
// can cancel the UniFFI future themselves.
func TestRunPassesCallerCancelWhenThereIsNoObjectCancel(t *testing.T) {
	ctx, cancelCtx := context.WithCancel(context.Background())
	defer cancelCtx()

	got := make(chan context.Context, 1)
	_, err := Call(ctx, nil, func(callCtx context.Context) (int, error) {
		got <- callCtx
		return 1, nil
	})
	if err != nil {
		t.Fatal(err)
	}
	callCtx := <-got
	cancelCtx()
	if callCtx.Err() == nil {
		t.Fatal("a call with no object cancel should see the caller's context")
	}
}

// Cancelling ctx while an object-owned call is blocked still runs cancel, which
// is what unblocks the native work once the generated binding no longer sees ctx.
func TestRunCancelsTheObjectWhenCtxEndsDuringTheCall(t *testing.T) {
	ctx, cancelCtx := context.WithCancel(context.Background())
	started := make(chan struct{})
	objectCancelled := make(chan struct{})
	returned := make(chan struct{})
	sawCallCtxDone := make(chan struct{})

	go func() {
		<-started
		cancelCtx()
	}()

	_, err := Call(ctx, func() { close(objectCancelled) }, func(callCtx context.Context) (int, error) {
		close(started)
		<-objectCancelled
		select {
		case <-callCtx.Done():
			close(sawCallCtxDone)
		default:
		}
		close(returned)
		return 0, nil
	})
	if !errors.Is(err, context.Canceled) {
		t.Fatalf("err = %v, want context.Canceled", err)
	}
	select {
	case <-returned:
	case <-time.After(5 * time.Second):
		t.Fatal("generated call never returned")
	}
	select {
	case <-sawCallCtxDone:
		t.Fatal("generated call saw the cancelled caller context")
	default:
	}
}

// A call that beats its context keeps its result: the reconciliation must not
// cost the caller a handle it did receive.
func TestRunHandleReturnsAResultThatWon(t *testing.T) {
	stub := &stubHandle{destroyed: make(chan struct{})}

	val, err := CallHandle(context.Background(), nil, func(context.Context) (*stubHandle, error) {
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

// A range loop whose context ends cancels the stream; one that ends on a
// stream error leaves it alone.
func TestSeqCancelsOnlyWhenContextEnds(t *testing.T) {
	failed := errors.New("failed")
	for _, tc := range []struct {
		name   string
		cancel bool
		err    error
	}{
		{"context", true, context.Canceled},
		{"stream", false, failed},
	} {
		t.Run(tc.name, func(t *testing.T) {
			ctx, stop := context.WithCancel(context.Background())
			defer stop()
			cancelled := false
			next := func(context.Context) (*int, error) {
				if tc.cancel {
					stop()
				}
				return nil, tc.err
			}
			for _, err := range Seq(ctx, func() { cancelled = true }, next) {
				if !errors.Is(err, tc.err) {
					t.Fatalf("err = %v, want %v", err, tc.err)
				}
			}
			if cancelled != tc.cancel {
				t.Fatalf("cancelled = %v, want %v", cancelled, tc.cancel)
			}
		})
	}
}
