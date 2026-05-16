package moq

import "context"

// callCtx bridges a blocking Moq* call (which uniffi-bindgen-go runs on the
// embedded tokio runtime) with context.Context cancellation. On ctx.Done()
// it calls cancel() which translates to MoqError::Cancelled inside the
// pending Rust call, then drains the result channel to avoid a goroutine
// leak.
func callCtx[T any](ctx context.Context, cancel func(), fn func() (T, error)) (T, error) {
	type result struct {
		v   T
		err error
	}
	ch := make(chan result, 1)
	go func() {
		v, err := fn()
		ch <- result{v, err}
	}()
	select {
	case <-ctx.Done():
		cancel()
		<-ch
		var zero T
		return zero, ctx.Err()
	case r := <-ch:
		return r.v, r.err
	}
}
